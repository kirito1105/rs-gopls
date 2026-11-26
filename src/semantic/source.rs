use std::cell::OnceCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use lsp_types::{Position, Range, TextDocumentContentChangeEvent};
use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

use crate::symbols::{self, FileOutline};

#[derive(Clone, Copy)]
pub(super) enum ReferenceQuery<'tree> {
    Name(Node<'tree>),
    Member {
        receiver: Node<'tree>,
        name: Node<'tree>,
    },
    LiteralKey {
        value: Node<'tree>,
        name: Node<'tree>,
    },
}

impl<'tree> ReferenceQuery<'tree> {
    pub(super) fn name(self) -> Node<'tree> {
        match self {
            Self::Name(name) | Self::Member { name, .. } | Self::LiteralKey { name, .. } => name,
        }
    }
}

pub(super) fn is_test_source(path: &Path) -> bool {
    path.file_name()
        .and_then(|filename| filename.to_str())
        .is_some_and(|filename| filename.ends_with("_test.go"))
}

pub(super) fn read_source(parser: &mut Parser, path: &Path) -> Result<Source> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Source::parse(parser, path.to_owned(), text, None)
}

#[derive(Clone)]
pub(super) struct Import {
    pub(super) alias: Option<String>,
    pub(super) path: String,
    pub(super) binding: Range,
}

pub(super) struct TextEditDelta<'a> {
    pub(super) range: Option<Range>,
    pub(super) bytes: std::ops::Range<usize>,
    pub(super) inserted: &'a str,
    pub(super) new_end_line: usize,
    pub(super) text: &'a str,
    pub(super) tree: &'a Tree,
}

pub(super) struct Source {
    pub(super) path: PathBuf,
    pub(super) text: String,
    pub(super) lines: LineIndex,
    pub(super) tree: Tree,
    pub(super) imports: Vec<Import>,
    pub(super) outline: OnceCell<FileOutline>,
    reference_names: OnceCell<HashMap<String, Vec<usize>>>,
}

impl Source {
    pub(super) fn parse(
        parser: &mut Parser,
        path: PathBuf,
        text: String,
        previous: Option<&Tree>,
    ) -> Result<Self> {
        let lines = LineIndex::new(&text);
        Self::parse_with_lines(parser, path, text, lines, previous)
    }

    fn parse_with_lines(
        parser: &mut Parser,
        path: PathBuf,
        text: String,
        lines: LineIndex,
        previous: Option<&Tree>,
    ) -> Result<Self> {
        let tree = parser.parse(&text, previous).context("parsing Go source")?;
        Ok(Self::from_tree(path, text, lines, tree))
    }

    pub(super) fn parse_declarations(
        parser: &mut Parser,
        path: PathBuf,
        text: String,
        bodies: &[std::ops::Range<usize>],
    ) -> Result<(Self, bool)> {
        let lines = LineIndex::new(&text);
        if !bodies.is_empty() {
            // Keep both braces and all original byte/UTF-16 offsets. The
            // declaration grammar and source text are otherwise unchanged.
            let mut start = 0;
            let mut included = Vec::with_capacity(bodies.len() + 1);
            for body in bodies {
                included.push(tree_sitter::Range {
                    start_byte: start,
                    end_byte: body.start,
                    start_point: lines.point(start),
                    end_point: lines.point(body.start),
                });
                start = body.end;
            }
            included.push(tree_sitter::Range {
                start_byte: start,
                end_byte: text.len(),
                start_point: lines.point(start),
                end_point: lines.point(text.len()),
            });
            if parser.set_included_ranges(&included).is_ok() {
                let tree = parser.parse(&text, None);
                parser
                    .set_included_ranges(&[])
                    .expect("resetting Tree-sitter input ranges");
                if let Some(tree) = tree.filter(|tree| !tree.root_node().has_error()) {
                    return Ok((Self::from_tree(path, text, lines, tree), true));
                }
            }
        }
        Self::parse_with_lines(parser, path, text, lines, None).map(|source| (source, false))
    }

    fn from_tree(path: PathBuf, text: String, lines: LineIndex, tree: Tree) -> Self {
        let imports = imports(tree.root_node(), &text, &lines);
        Self {
            path,
            text,
            lines,
            tree,
            imports,
            outline: OnceCell::new(),
            reference_names: OnceCell::new(),
        }
    }

    // The observer sees each validated change against the current working copy.
    // Its decisions should be applied only after the entire batch succeeds.
    pub(super) fn edit(
        &mut self,
        parser: &mut Parser,
        changes: &[TextDocumentContentChangeEvent],
        mut observe: Option<&mut dyn FnMut(TextEditDelta<'_>) -> bool>,
    ) -> Result<()> {
        let mut text = self.text.clone();
        let mut tree = self.tree.clone();
        let mut lines = self.lines.clone();
        for change in changes {
            let (start, end) = if let Some(range) = change.range {
                (
                    lines
                        .offset(&text, range.start)
                        .context("invalid edit start")?,
                    lines.offset(&text, range.end).context("invalid edit end")?,
                )
            } else {
                (0, text.len())
            };
            if start > end {
                bail!("edit ends before it begins");
            }

            let start_position = lines.point(start);
            let new_end_position = match change.text.rfind('\n') {
                Some(last_break) => Point::new(
                    start_position.row + change.text.bytes().filter(|byte| *byte == b'\n').count(),
                    change.text.len() - last_break - 1,
                ),
                None => Point::new(
                    start_position.row,
                    start_position.column + change.text.len(),
                ),
            };
            if let Some(observer) = &mut observe
                && !observer(TextEditDelta {
                    range: change.range,
                    bytes: start..end,
                    inserted: &change.text,
                    new_end_line: new_end_position.row,
                    text: &text,
                    tree: &tree,
                })
            {
                observe = None;
            }
            tree.edit(&InputEdit {
                start_byte: start,
                old_end_byte: end,
                new_end_byte: start + change.text.len(),
                start_position,
                old_end_position: lines.point(end),
                new_end_position,
            });
            lines.edit(start, end, &change.text);
            text.replace_range(start..end, &change.text);
        }

        *self = Self::parse_with_lines(parser, self.path.clone(), text, lines, Some(&tree))?;
        Ok(())
    }

    pub(super) fn reference_at(&self, position: Position) -> Option<ReferenceQuery<'_>> {
        let offset = self.lines.offset(&self.text, position)?;
        self.reference_at_offset(offset)
    }

    pub(super) fn reference_at_offset(&self, offset: usize) -> Option<ReferenceQuery<'_>> {
        let root = self.tree.root_node();
        let leaf = root.named_descendant_for_byte_range(offset, offset)?;
        self.reference_from_leaf(leaf, offset)
    }

    pub(super) fn for_each_name<'tree>(
        &self,
        root: Node<'tree>,
        spelling: &str,
        mut visit: impl FnMut(Node<'tree>),
    ) {
        Self::for_each_identifier(root, |node| {
            if node_text(node, &self.text) == spelling {
                visit(node);
            }
        });
    }

    pub(super) fn name_offsets(&self, spelling: &str) -> &[usize] {
        self.reference_names
            .get_or_init(|| {
                let mut names = HashMap::<String, Vec<usize>>::new();
                Self::for_each_identifier(self.tree.root_node(), |node| {
                    let name = node_text(node, &self.text);
                    if let Some(offsets) = names.get_mut(name) {
                        offsets.push(node.start_byte());
                    } else {
                        names.insert(name.to_owned(), vec![node.start_byte()]);
                    }
                });
                names
            })
            .get(spelling)
            .map_or(&[], Vec::as_slice)
    }

    fn for_each_identifier<'tree>(root: Node<'tree>, mut visit: impl FnMut(Node<'tree>)) {
        let mut cursor = root.walk();
        loop {
            let node = cursor.node();
            if matches!(
                node.kind(),
                "identifier" | "type_identifier" | "field_identifier" | "package_identifier"
            ) {
                visit(node);
            }
            if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    return;
                }
            }
        }
    }

    pub(super) fn reference_from_leaf<'tree>(
        &self,
        leaf: Node<'tree>,
        offset: usize,
    ) -> Option<ReferenceQuery<'tree>> {
        let mut current = Some(leaf);
        while let Some(node) = current {
            let fields = match node.kind() {
                "selector_expression" => Some(("operand", "field")),
                "qualified_type" => Some(("package", "name")),
                _ => None,
            };
            if let Some((receiver_field, name_field)) = fields
                && let Some(name) = node.child_by_field_name(name_field)
                && name.start_byte() <= offset
                && offset <= name.end_byte()
            {
                let receiver = node.child_by_field_name(receiver_field)?;
                return Some(ReferenceQuery::Member { receiver, name });
            }
            if node.kind() == "import_spec" {
                if let Some(path) = node.child_by_field_name("path")
                    && path.start_byte() <= offset
                    && offset < path.end_byte()
                {
                    return Some(ReferenceQuery::Name(path));
                }
                break;
            }
            if node.kind() == "comment" {
                return None;
            }
            current = node.parent();
        }
        let parent = leaf.parent()?;
        let named = matches!(
            leaf.kind(),
            "identifier" | "type_identifier" | "package_identifier"
        );
        let field_key = parent.kind() == "literal_element"
            && parent.parent().is_some_and(|node| {
                node.kind() == "keyed_element" && node.child_by_field_name("key") == Some(parent)
            });
        if named && field_key {
            let value = parent.parent()?.parent()?;
            if value.kind() == "literal_value" {
                return Some(ReferenceQuery::LiteralKey { value, name: leaf });
            }
        }
        let predeclared_literal = matches!(leaf.kind(), "nil" | "true" | "false" | "iota");
        (named || predeclared_literal || is_declaration_name(leaf))
            .then_some(ReferenceQuery::Name(leaf))
    }
    pub(super) fn range(&self, node: Node<'_>) -> Range {
        Range::new(
            position_on_line(&self.text, node.start_position(), node.start_byte()),
            position_on_line(&self.text, node.end_position(), node.end_byte()),
        )
    }

    pub(super) fn byte_range(&self, range: std::ops::Range<usize>) -> Range {
        Range::new(
            self.lines.position(&self.text, range.start),
            self.lines.position(&self.text, range.end),
        )
    }

    pub(super) fn outline(&self) -> &FileOutline {
        self.outline
            .get_or_init(|| symbols::collect_outline(&self.tree, &self.text))
    }
}

pub(super) fn import_path(literal: &str) -> Option<String> {
    if let Some(raw) = literal.strip_prefix('`').and_then(|s| s.strip_suffix('`')) {
        Some(raw.replace('\r', ""))
    } else {
        serde_json::from_str(literal).ok()
    }
}

pub(super) fn import_specs(declaration: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut cursor = declaration.walk();
    let container = declaration
        .named_children(&mut cursor)
        .find(|child| child.kind() == "import_spec_list")
        .unwrap_or(declaration);
    std::iter::successors(container.named_child(0), Node::next_named_sibling)
        .filter(|child| child.kind() == "import_spec")
}

// String escapes in Go include byte escapes, which can encode one piece of a
// UTF-8 character at a time. Decode before comparing import paths.
pub(super) fn decoded_import_path(literal: &str) -> Option<String> {
    if let Some(raw) = literal.strip_prefix('`').and_then(|s| s.strip_suffix('`')) {
        return Some(raw.replace('\r', ""));
    }
    let mut contents = literal.strip_prefix('"')?.strip_suffix('"')?;
    let mut decoded = Vec::with_capacity(contents.len());
    while let Some((prefix, escape)) = contents.split_once('\\') {
        decoded.extend_from_slice(prefix.as_bytes());
        let (kind, rest) = escape.split_at_checked(1)?;
        let (digits, unicode) = match kind.as_bytes()[0] {
            b'x' => (2, false),
            b'u' => (4, true),
            b'U' => (8, true),
            b'0'..=b'7' => {
                let (digits, rest) = escape.split_at_checked(3)?;
                decoded.push(u8::from_str_radix(digits, 8).ok()?);
                contents = rest;
                continue;
            }
            _ => {
                decoded.push(match kind {
                    "a" => 7,
                    "b" => 8,
                    "f" => 12,
                    "n" => b'\n',
                    "r" => b'\r',
                    "t" => b'\t',
                    "v" => 11,
                    "\\" => b'\\',
                    "\"" => b'"',
                    _ => return None,
                });
                contents = rest;
                continue;
            }
        };
        let (digits, rest) = rest.split_at_checked(digits)?;
        if !digits.bytes().all(|digit| digit.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(digits, 16).ok()?;
        if unicode {
            decoded.extend_from_slice(char::from_u32(value)?.encode_utf8(&mut [0; 4]).as_bytes());
        } else {
            decoded.push(u8::try_from(value).ok()?);
        }
        contents = rest;
    }
    decoded.extend_from_slice(contents.as_bytes());
    String::from_utf8(decoded).ok()
}

pub(super) fn raw_imports<'a>(
    root: Node<'a>,
    text: &'a str,
    lines: &'a LineIndex,
) -> impl Iterator<Item = Import> + 'a {
    std::iter::successors(root.named_child(0), Node::next_named_sibling)
        .filter(|child| child.kind() == "import_declaration")
        .flat_map(import_specs)
        .filter(|spec| !spec.has_error())
        .filter_map(move |spec| import_from_spec(spec, text, lines, decoded_import_path))
}

fn import_from_spec(
    spec: Node<'_>,
    text: &str,
    lines: &LineIndex,
    decode: fn(&str) -> Option<String>,
) -> Option<Import> {
    let path = spec.child_by_field_name("path")?;
    let alias = spec.child_by_field_name("name");
    let name = alias.unwrap_or(path);
    Some(Import {
        alias: alias.map(|node| node_text(node, text).to_owned()),
        path: decode(node_text(path, text))?,
        binding: Range::new(
            lines.position(text, name.start_byte()),
            lines.position(text, name.end_byte()),
        ),
    })
}

fn imports(root: Node<'_>, text: &str, lines: &LineIndex) -> Vec<Import> {
    fn collect(node: Node<'_>, text: &str, lines: &LineIndex, result: &mut Vec<Import>) {
        match node.kind() {
            "import_declaration" | "import_spec_list" => {
                for child in node.named_children(&mut node.walk()) {
                    collect(child, text, lines, result);
                }
            }
            "import_spec" => {
                if let Some(import) = import_from_spec(node, text, lines, import_path)
                    && import.alias.as_deref() != Some("_")
                {
                    result.push(import);
                }
            }
            _ => {}
        }
    }

    let mut result = Vec::new();
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        collect(node, text, lines, &mut result);
    }
    result
}

pub(super) fn is_type_declaration(node: Node<'_>) -> bool {
    matches!(node.kind(), "type_spec" | "type_alias")
}

pub(super) fn is_declaration_name(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() == "import_spec" {
        return parent.child_by_field_name("path") == Some(node);
    }
    if !matches!(
        parent.kind(),
        "field_declaration"
            | "function_declaration"
            | "method_declaration"
            | "method_elem"
            | "parameter_declaration"
            | "variadic_parameter_declaration"
            | "type_parameter_declaration"
            | "type_alias"
            | "type_spec"
            | "const_spec"
            | "var_spec"
    ) {
        return false;
    }
    let mut cursor = parent.walk();
    parent
        .children_by_field_name("name", &mut cursor)
        .any(|name| name == node)
}

pub(super) fn node_text<'text>(node: Node<'_>, text: &'text str) -> &'text str {
    &text[node.byte_range()]
}

#[derive(Clone)]
pub(super) struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub(super) fn new(text: &str) -> Self {
        let starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(index, _)| index + 1))
            .collect();
        Self { starts }
    }

    pub(super) fn offset(&self, text: &str, position: Position) -> Option<usize> {
        let line = position.line as usize;
        let start = *self.starts.get(line)?;
        let end = self
            .starts
            .get(line + 1)
            .map_or(text.len(), |next| next - 1);
        let content = &text[start..end];
        let content = content.strip_suffix('\r').unwrap_or(content);
        let character = position.character as usize;
        if content.is_ascii() {
            return Some(start + character.min(content.len()));
        }
        let mut units = 0;
        for (index, ch) in content.char_indices() {
            if units == character {
                return Some(start + index);
            }
            units += ch.len_utf16();
            if units > character {
                return None;
            }
        }
        Some(start + content.len())
    }

    pub(super) fn point(&self, offset: usize) -> Point {
        let line = self.starts.partition_point(|start| *start <= offset) - 1;
        Point::new(line, offset - self.starts[line])
    }

    pub(super) fn position(&self, text: &str, offset: usize) -> Position {
        position_on_line(text, self.point(offset), offset)
    }

    pub(super) fn edit(&mut self, start: usize, end: usize, inserted: &str) {
        let first = self.starts.partition_point(|offset| *offset <= start);
        let last = self.starts.partition_point(|offset| *offset <= end);
        for offset in &mut self.starts[last..] {
            *offset = *offset - (end - start) + inserted.len();
        }
        self.starts.splice(
            first..last,
            inserted
                .match_indices('\n')
                .map(|(index, _)| start + index + 1),
        );
    }
}

fn position_on_line(text: &str, point: Point, offset: usize) -> Position {
    let prefix = &text[offset - point.column..offset];
    let prefix = if text.as_bytes().get(offset) == Some(&b'\n') {
        prefix.strip_suffix('\r').unwrap_or(prefix)
    } else {
        prefix
    };
    let column = if prefix.is_ascii() {
        prefix.len()
    } else {
        prefix.encode_utf16().count()
    };
    Position::new(point.row as u32, column as u32)
}

pub(super) fn function_at(source: &Source, origin: Range) -> Option<Node<'_>> {
    let node = node_at_range(source, origin)?.parent()?;
    matches!(
        node.kind(),
        "function_declaration"
            | "method_declaration"
            | "method_elem"
            | "function_type"
            | "func_literal"
    )
    .then_some(node)
}

pub(super) fn node_at_range(source: &Source, range: Range) -> Option<Node<'_>> {
    let start = source.lines.offset(&source.text, range.start)?;
    let end = source.lines.offset(&source.text, range.end)?;
    let node = source
        .tree
        .root_node()
        .descendant_for_byte_range(start, end)?;
    (node.byte_range() == (start..end)).then_some(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::lexical_binding;

    fn parser() -> Parser {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();
        parser
    }

    fn change(range: Option<Range>, text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range,
            range_length: None,
            text: text.into(),
        }
    }

    #[test]
    fn import_qualifier_respects_shadowing_and_utf16() {
        let text = concat!(
            "package test\n",
            "import alias \"example.test/lib\"\n",
            "func a() { _ = \"😀\"; alias.Foo() }\n",
            "func b(alias T) { alias.Foo() }\n",
            "func c() { alias := T{}; alias.Foo() }\n",
            "func d() { if alias := T{}; true { alias.Foo() }; alias.Foo() }\n",
        );
        let source = Source::parse(&mut parser(), PathBuf::new(), text.into(), None).unwrap();
        let queries: Vec<_> = text
            .match_indices("Foo")
            .map(|(offset, _)| {
                let query = source.reference_at(source.lines.position(text, offset))?;
                let ReferenceQuery::Member { receiver, name } = query else {
                    return None;
                };
                let qualifier = node_text(receiver, text);
                lexical_binding(receiver, qualifier, text)
                    .is_none()
                    .then_some((qualifier, node_text(name, text)))
            })
            .collect();
        assert_eq!(
            queries,
            [
                Some(("alias", "Foo")),
                None,
                None,
                None,
                Some(("alias", "Foo"))
            ],
        );
        assert!(source.lines.offset(text, Position::new(2, 17)).is_none());
    }

    #[test]
    fn line_index_tracks_multiline_insertions_and_deletions() {
        for (text, start, end, insert) in [
            ("abc\ndef\n", 2, 6, "\n\n"),
            ("abc\ndef\n", 4, 4, "new\n"),
            ("abc\ndef\n", 0, 4, ""),
            ("abc\ndef\n", 0, 8, "single"),
            ("", 0, 0, "\r\n"),
        ] {
            let mut lines = LineIndex::new(text);
            lines.edit(start, end, insert);
            let mut edited = text.to_owned();
            edited.replace_range(start..end, insert);
            assert_eq!(lines.starts, LineIndex::new(&edited).starts);
        }
    }

    #[test]
    fn observes_batch_coordinates_in_utf16_and_preserves_crlf() {
        let mut parser = parser();
        let original = "package p\r\nvar S = \"😀\"\r\nvar Tail int\r\n";
        let mut source = Source::parse(&mut parser, PathBuf::new(), original.into(), None).unwrap();
        let after_insertion = "package p\r\n// top 😀\r\nvar S = \"😀\"\r\nvar Tail int\r\n";
        let after_emoji = "package p\r\n// top 😀\r\nvar S = \"x\"\r\nvar Tail int\r\n";
        let range = |line, from, to| {
            Some(Range::new(
                Position::new(line, from),
                Position::new(line, to),
            ))
        };
        let changes = [
            change(range(1, 0, 0), "// top 😀\r\n"),
            change(range(2, 9, 11), "x"),
            change(range(2, 99, 99), " // tail"),
        ];
        let expected = [
            (original, original.find("var S").unwrap(), "", 2),
            (
                after_insertion,
                after_insertion.find("\"😀").unwrap() + 1,
                "😀",
                2,
            ),
            (after_emoji, after_emoji.find("\"\r\n").unwrap() + 1, "", 2),
        ];
        let mut observed = 0;
        source
            .edit(
                &mut parser,
                &changes,
                Some(&mut |delta| {
                    let (before, start, replaced, end_line) = expected[observed];
                    assert_eq!(delta.range, changes[observed].range);
                    assert_eq!(delta.inserted, changes[observed].text);
                    assert_eq!(delta.text, before);
                    assert_eq!(delta.bytes, start..start + replaced.len());
                    assert_eq!(&delta.text[delta.bytes], replaced);
                    assert_eq!(delta.new_end_line, end_line);
                    assert_eq!(delta.tree.root_node().end_byte(), before.len());
                    observed += 1;
                    true
                }),
            )
            .unwrap();
        assert_eq!(observed, changes.len());
        assert_eq!(
            source.text,
            "package p\r\n// top 😀\r\nvar S = \"x\" // tail\r\nvar Tail int\r\n"
        );
        assert!(!source.tree.root_node().has_error());
        assert_eq!(source.lines.starts, LineIndex::new(&source.text).starts);
    }

    #[test]
    fn rejects_a_surrogate_boundary_without_committing_earlier_edits() {
        let mut parser = parser();
        let original = "package p\nvar S = \"😀\"\n";
        let mut source = Source::parse(&mut parser, PathBuf::new(), original.into(), None).unwrap();
        let original_root = source.tree.root_node().id();
        let original_outline = std::ptr::from_ref(source.outline());
        let range = |line, character| {
            let position = Position::new(line, character);
            Some(Range::new(position, position))
        };
        let mut observed = 0;
        let error = source
            .edit(
                &mut parser,
                &[change(range(1, 0), "\n"), change(range(2, 10), "invalid")],
                Some(&mut |_| {
                    observed += 1;
                    true
                }),
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "invalid edit start");
        assert_eq!(observed, 1);
        assert_eq!(source.text, original);
        assert_eq!(source.tree.root_node().id(), original_root);
        assert_eq!(std::ptr::from_ref(source.outline()), original_outline);
        assert_eq!(source.lines.starts, LineIndex::new(original).starts);
    }

    #[test]
    fn reports_full_replacement_and_honors_the_observers_stop() {
        let mut parser = parser();
        let original = "package p\nvar Original int\n";
        let mut source = Source::parse(&mut parser, PathBuf::new(), original.into(), None).unwrap();
        let full_text = "package p\nvar Updated int\n";
        let changes = [
            change(None, full_text),
            change(
                Some(Range::new(Position::new(1, 4), Position::new(1, 11))),
                "Final",
            ),
        ];
        let mut observed = 0;
        source
            .edit(
                &mut parser,
                &changes,
                Some(&mut |delta| {
                    assert_eq!(delta.range, None);
                    assert_eq!(delta.bytes, 0..original.len());
                    assert_eq!(delta.inserted, full_text);
                    assert_eq!(delta.new_end_line, 2);
                    observed += 1;
                    false
                }),
            )
            .unwrap();
        assert_eq!(observed, 1);
        assert_eq!(source.text, "package p\nvar Final int\n");
        assert!(!source.tree.root_node().has_error());
    }
}
