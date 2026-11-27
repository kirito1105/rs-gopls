use std::cmp::Reverse;

use lsp_types::{FoldingRange, FoldingRangeKind, Position};
use tree_sitter::Node;

use super::{EditorFoldingOptions, named_children};
use crate::semantic::snapshot::DocumentSnapshot;

struct Fold<T> {
    start: T,
    end: T,
    kind: Option<FoldingRangeKind>,
}

impl Fold<usize> {
    fn new(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            kind: None,
        }
    }

    fn with_kind(mut self, kind: FoldingRangeKind) -> Self {
        self.kind = Some(kind);
        self
    }

    fn project(self, document: &DocumentSnapshot<'_>, lines_only: bool) -> Option<Fold<Position>> {
        let range = document.byte_range(self.start..self.end);
        let mut fold = Fold {
            start: range.start,
            end: range.end,
            kind: self.kind,
        };
        if lines_only {
            // The byte range is half-open, including when it ends at a line break.
            if fold.end.character == 0 {
                fold.end.line = fold.end.line.checked_sub(1)?;
            }
            let line_start = document.offset(Position::new(fold.end.line, 0))?;
            let line = document.text()[line_start..].split('\n').next()?;
            if matches!(line.trim(), "}" | ")") {
                fold.end.line = fold.end.line.checked_sub(1)?;
            }
            fold.start.character = 0;
            fold.end.character = 0;
        }
        (fold.start < fold.end).then_some(fold)
    }
}

pub(in crate::semantic) fn folding_ranges(
    document: &DocumentSnapshot<'_>,
    options: EditorFoldingOptions,
) -> Vec<FoldingRange> {
    let root = document.root_node();
    let text = document.text();
    let mut folds = Vec::new();
    let mut region_groups = Vec::new();
    folds.extend(imports(root, text));
    let mut nodes = vec![root];
    while let Some(node) = nodes.pop() {
        folds.extend(structure(node, document));
        let markers = comment_folds(node, text, &mut folds);
        if !markers.is_empty() {
            region_groups.push(markers);
        }
        let mut cursor = node.walk();
        nodes.extend(node.named_children(&mut cursor));
    }
    region_folds(region_groups, &mut folds);

    let projected = nested_folds(folds)
        .into_iter()
        .filter_map(|fold| fold.project(document, options.line_folding_only))
        .collect();
    nested_folds(projected)
        .into_iter()
        .map(|fold| FoldingRange {
            start_line: fold.start.line,
            start_character: (!options.line_folding_only).then_some(fold.start.character),
            end_line: fold.end.line,
            end_character: (!options.line_folding_only).then_some(fold.end.character),
            kind: fold.kind,
            collapsed_text: None,
        })
        .collect()
}

fn imports(root: Node<'_>, text: &str) -> Option<Fold<usize>> {
    let mut cursor = root.walk();
    let mut declarations = root
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "import_declaration");
    let first = declarations.next()?;
    let last = match declarations.last() {
        Some(last) => last,
        None if crosses_lines(first) => first,
        None => return None,
    };
    if let Some(list) = child(last, "import_spec_list") {
        delimited(list, "(", ")")?;
    }
    let mut start = child(first, "import")?.end_byte();
    if let Some(whitespace) = text[start..].chars().next().filter(|ch| ch.is_whitespace()) {
        start += whitespace.len_utf8();
    }
    let end = last.end_byte();
    (text.get(start..end)?.encode_utf16().take(4).count() > 3)
        .then(|| Fold::new(start, end).with_kind(FoldingRangeKind::Imports))
}

fn structure(node: Node<'_>, document: &DocumentSnapshot<'_>) -> Option<Fold<usize>> {
    let text = document.text();
    match node.kind() {
        "block" if crosses_lines(node) => delimited(node, "{", "}"),
        "expression_switch_statement"
        | "type_switch_statement"
        | "select_statement"
        | "field_declaration_list"
        | "interface_type" => delimited(node, "{", "}"),
        "expression_case" | "type_case" | "communication_case" | "default_case" => {
            case_fold(node, document)
        }
        "var_spec_list" | "const_declaration" | "type_declaration" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .filter(|child| {
                    matches!(
                        child.kind(),
                        "var_spec" | "const_spec" | "type_spec" | "type_alias"
                    )
                })
                .nth(1)?;
            delimited(node, "(", ")")
        }
        "literal_value"
            if crosses_lines(node)
                && (child(node, "literal_element").is_some()
                    || child(node, "keyed_element").is_some()) =>
        {
            delimited(node, "{", "}")
        }
        "raw_string_literal" if crosses_lines(node) => delimited(node, "`", "`"),
        "comment" => {
            let comment = text.get(node.byte_range())?;
            (comment.starts_with("/*") && comment.ends_with("*/") && comment.len() > 2).then(|| {
                Fold::new(node.start_byte(), node.end_byte()).with_kind(FoldingRangeKind::Comment)
            })
        }
        _ => None,
    }
}

fn case_fold(node: Node<'_>, document: &DocumentSnapshot<'_>) -> Option<Fold<usize>> {
    const DEFAULT_RIGHT_MARGIN: usize = 120;
    const DEFAULT_TAB_SIZE: usize = 4;

    let colon = child(node, ":")?;
    let mut statements = named_children(child(node, "statement_list")?);
    let first = statements.next()?;
    let text = document.text();
    if statements.next().is_none() && !crosses_lines(first) {
        let start = colon.start_byte();
        let line = document.byte_range(start..start).start.line;
        let line_start = document.offset(Position::new(line, 0))?;
        let column = text[line_start..start].chars().fold(0, |column, ch| {
            if ch == '\t' {
                column + DEFAULT_TAB_SIZE - column % DEFAULT_TAB_SIZE
            } else {
                column + 1
            }
        });
        let statement_width = text[first.byte_range()].encode_utf16().count();
        if column + ": ".len() + statement_width <= DEFAULT_RIGHT_MARGIN {
            return Some(Fold::new(start, first.start_byte()));
        }
    }
    // Tree-sitter's clause can include a trailing newline consumed by the
    // statement list. GoLand's clause range ends at the last non-whitespace token.
    let end = node.start_byte() + text[node.byte_range()].trim_end().len();
    Some(Fold::new(colon.end_byte(), end))
}

fn crosses_lines(node: Node<'_>) -> bool {
    node.start_position().row < node.end_position().row
}

fn child<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|child| child.kind() == kind && !child.is_missing());
    found
}

fn delimited(node: Node<'_>, opening: &str, closing: &str) -> Option<Fold<usize>> {
    let start = child(node, opening)?.start_byte();
    let end = (0..node.child_count()).rev().find_map(|index| {
        let child = node.child(index)?;
        (child.kind() == closing && !child.is_missing() && child.start_byte() > start)
            .then_some(child.end_byte())
    })?;
    Some(Fold::new(start, end))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RegionStyle {
    Region,
    EditorFold,
}

struct RegionMarker {
    style: RegionStyle,
    opening: bool,
    start: usize,
    end: usize,
}

fn region_marker(comment: &str, start: usize, end: usize) -> Option<RegionMarker> {
    let body = comment
        .trim_start_matches(['/', '*', '#', '-'])
        .trim_start();
    let (style, opening) = if body.starts_with("region") && !body.contains(['\r', '\n']) {
        (RegionStyle::Region, true)
    } else if body.starts_with("endregion") && !body.contains(['\r', '\n']) {
        (RegionStyle::Region, false)
    } else if comment.contains("<editor-fold") {
        (RegionStyle::EditorFold, true)
    } else if comment.contains("</editor-fold") {
        (RegionStyle::EditorFold, false)
    } else {
        return None;
    };
    Some(RegionMarker {
        style,
        opening,
        start,
        end,
    })
}

struct CommentGroup {
    start: usize,
    end: usize,
    count: usize,
}

impl CommentGroup {
    fn fold(self) -> Option<Fold<usize>> {
        (self.count > 1)
            .then(|| Fold::new(self.start, self.end).with_kind(FoldingRangeKind::Comment))
    }
}

fn comment_folds(node: Node<'_>, text: &str, folds: &mut Vec<Fold<usize>>) -> Vec<RegionMarker> {
    let mut cursor = node.walk();
    let mut group: Option<CommentGroup> = None;
    let mut regions = Vec::new();
    for comment in node
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "comment")
    {
        let comment_text = &text[comment.byte_range()];
        let start = comment.start_byte();
        let end = comment.end_byte();
        if comment_text.starts_with("//") {
            if let Some(group) = group.as_mut().filter(|group| {
                let gap = &text[group.end..start];
                gap.chars().all(char::is_whitespace)
                    && gap.bytes().filter(|&byte| byte == b'\n').count() <= 1
            }) {
                group.end = end;
                group.count += 1;
            } else {
                folds.extend(group.take().and_then(CommentGroup::fold));
                group = Some(CommentGroup {
                    start,
                    end,
                    count: 1,
                });
            }
        } else {
            folds.extend(group.take().and_then(CommentGroup::fold));
        }

        regions.extend(region_marker(
            comment_text.trim_end_matches('\r'),
            start,
            end,
        ));
    }
    folds.extend(group.and_then(CommentGroup::fold));
    regions
}

fn region_folds(groups: Vec<Vec<RegionMarker>>, folds: &mut Vec<Fold<usize>>) {
    // The first marker in the file selects a provider, even if it is an
    // unmatched end marker. Pair markers within their own syntax parent.
    let Some(first) = groups
        .iter()
        .filter_map(|markers| markers.first())
        .min_by_key(|marker| marker.start)
    else {
        return;
    };
    let style = first.style;
    for markers in groups {
        let mut starts = Vec::new();
        for marker in markers.into_iter().filter(|marker| marker.style == style) {
            if marker.opening {
                starts.push(marker.start);
            } else if let Some(start) = starts.pop() {
                folds.push(Fold::new(start, marker.end).with_kind(FoldingRangeKind::Region));
            }
        }
    }
}

fn kind_priority(kind: Option<&FoldingRangeKind>) -> u8 {
    match kind {
        Some(FoldingRangeKind::Region) => 0,
        Some(FoldingRangeKind::Imports) => 1,
        Some(FoldingRangeKind::Comment) => 2,
        None => 3,
    }
}

fn nested_folds<T: Copy + Ord>(mut folds: Vec<Fold<T>>) -> Vec<Fold<T>> {
    folds.sort_by_key(|fold| {
        (
            fold.start,
            Reverse(fold.end),
            kind_priority(fold.kind.as_ref()),
        )
    });
    let mut accepted: Vec<Option<Fold<T>>> = Vec::new();
    let mut parents: Vec<usize> = Vec::new();
    'candidate: for fold in folds {
        if fold.start >= fold.end {
            continue;
        }
        while parents.last().is_some_and(|&index| {
            accepted[index]
                .as_ref()
                .is_some_and(|parent| parent.end <= fold.start)
        }) {
            parents.pop();
        }

        let mut keep = parents.len();
        for &index in parents.iter().rev() {
            let parent = accepted[index].as_ref().expect("active fold was accepted");
            if fold.start == parent.start && fold.end == parent.end {
                continue 'candidate;
            }
            if fold.end <= parent.end {
                break;
            }
            // Replace crossing comment groups only if the region fits the remaining folds.
            if fold.kind == Some(FoldingRangeKind::Region)
                && parent.kind == Some(FoldingRangeKind::Comment)
            {
                keep -= 1;
            } else {
                continue 'candidate;
            }
        }
        for index in parents.drain(keep..) {
            accepted[index] = None;
        }
        parents.push(accepted.len());
        accepted.push(Some(fold));
    }
    accepted.into_iter().flatten().collect()
}
