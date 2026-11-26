use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Range;

use lsp_types::TextEdit;
use tree_sitter::Node;

use crate::semantic::snapshot::DocumentSnapshot;
use crate::semantic::source::{import_path, node_text};

#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum ImportOperation<'a> {
    AddImport {
        path: &'a str,
        alias: Option<&'a str>,
    },
    OrganizeImports {
        unused_paths: &'a [&'a str],
        local_prefixes: &'a [&'a str],
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ImportEditError {
    MissingPackageClause,
    InvalidImportPath,
    InvalidImportAlias,
    UnsafeSyntaxTree,
}

impl fmt::Display for ImportEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissingPackageClause => "missing Go package clause",
            Self::InvalidImportPath => "invalid Go import path",
            Self::InvalidImportAlias => "invalid Go import alias",
            Self::UnsafeSyntaxTree => "cannot safely edit the Go package or import declarations",
        })
    }
}

impl std::error::Error for ImportEditError {}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn import_edits(
    document: &DocumentSnapshot<'_>,
    operation: ImportOperation<'_>,
) -> Result<Vec<TextEdit>, ImportEditError> {
    let text = document.text();
    let eol = match text.split_once('\n') {
        Some((first, _)) if first.ends_with('\r') => "\r\n",
        _ => "\n",
    };
    let mut header = Header::parse(document)?;
    match operation {
        ImportOperation::AddImport { path, alias } => {
            if !valid_path(path) {
                return Err(ImportEditError::InvalidImportPath);
            }
            if alias.is_some_and(|name| !valid_alias(name)) {
                return Err(ImportEditError::InvalidImportAlias);
            }
            if header
                .declarations
                .iter()
                .flat_map(|d| &d.block.specs)
                .any(|s| s.equivalent(path, alias))
            {
                return Ok(Vec::new());
            }

            let mut spec = Spec::new(path.to_owned(), alias, format!("\"{path}\"").into());
            let target = header
                .declarations
                .iter()
                .position(|d| d.grouped && !d.cgo())
                .or_else(|| header.declarations.iter().position(|d| !d.cgo()));
            if path != "C"
                && let Some(index) = target
            {
                let decl = &mut header.declarations[index];
                spec.group = insertion_group(path, &decl.block.specs);
                decl.block.specs.push(spec);
                decl.block.sort();
                let replacement = decl.block.render(eol, decl.grouped);
                return Ok(replace(document, decl.bytes.clone(), replacement, eol)
                    .into_iter()
                    .collect());
            }

            let end = header
                .declarations
                .last()
                .map_or(header.package_end, |d| d.bytes.end);
            let gap = whitespace_after(text, end);
            let mut replacement = format!("{eol}{eol}import {}{eol}", spec.line(&[]));
            if gap.end < text.len() {
                replacement.push_str(eol);
            }
            Ok(vec![TextEdit::new(document.byte_range(gap), replacement)])
        }
        ImportOperation::OrganizeImports {
            unused_paths,
            local_prefixes,
        } => {
            let unused: HashSet<_> = unused_paths.iter().copied().collect();
            let redundant = redundant_aliases(&header, &unused);
            let mut merged = ImportBlock::default();
            let mut sections: Vec<Range<usize>> = Vec::new();
            for decl in header.declarations {
                if decl.cgo() {
                    continue;
                }
                if let Some(previous) = sections.last_mut()
                    && text[previous.end..decl.bytes.start].trim().is_empty()
                {
                    previous.end = decl.bytes.end;
                } else {
                    sections.push(decl.bytes);
                }
                merged.leading.extend(decl.block.leading);
                merged.footer.extend(decl.block.footer);
                merged.trailing.extend(decl.block.trailing);
                for mut spec in decl.block.specs {
                    if spec.is_unused(&unused) {
                        continue;
                    }
                    if redundant.contains(&spec.path)
                        && spec.alias == Some(assumed_name(&spec.path))
                    {
                        spec.alias = None;
                    }
                    spec.group = classify(&spec.path, local_prefixes);
                    merged.specs.push(spec);
                }
            }
            merged.sort();
            merged.specs.dedup_by(|later, first| {
                if first.path == later.path && first.alias == later.alias {
                    first.leading.append(&mut later.leading);
                    first.trailing.append(&mut later.trailing);
                    true
                } else {
                    false
                }
            });
            let mut replacement = merged.render(eol, false);
            Ok(sections
                .into_iter()
                .filter_map(|bytes| replace(document, bytes, std::mem::take(&mut replacement), eol))
                .collect())
        }
    }
}

struct Header<'a> {
    package_end: usize,
    declarations: Vec<Declaration<'a>>,
}

impl<'a> Header<'a> {
    fn parse(document: &'a DocumentSnapshot<'_>) -> Result<Self, ImportEditError> {
        let text = document.text();
        let root = document.root_node();
        if root.is_error() {
            return Err(ImportEditError::UnsafeSyntaxTree);
        }
        let mut package_end = None;
        let mut declarations = Vec::new();
        let mut previous_end = 0;
        let mut in_body = false;
        for node in root.children(&mut root.walk()) {
            if !in_body && (node.is_missing() || node.is_error()) {
                return Err(ImportEditError::UnsafeSyntaxTree);
            }
            if !node.is_named() {
                continue;
            }
            match node.kind() {
                "comment" => continue,
                "package_clause" => {
                    if in_body || package_end.is_some() || node.has_error() {
                        return Err(ImportEditError::UnsafeSyntaxTree);
                    }
                    (previous_end, _) = declaration_tail(node, text);
                    package_end = Some(previous_end);
                }
                "import_declaration" => {
                    if in_body || node.has_error() {
                        return Err(ImportEditError::UnsafeSyntaxTree);
                    }
                    package_end.ok_or(ImportEditError::MissingPackageClause)?;
                    let decl = Declaration::parse(node, text, previous_end)?;
                    previous_end = decl.bytes.end;
                    declarations.push(decl);
                }
                _ => in_body = true,
            }
        }
        Ok(Self {
            package_end: package_end.ok_or(ImportEditError::MissingPackageClause)?,
            declarations,
        })
    }
}

struct Declaration<'a> {
    bytes: Range<usize>,
    grouped: bool,
    block: ImportBlock<'a>,
}

impl<'a> Declaration<'a> {
    fn cgo(&self) -> bool {
        self.block.specs.iter().any(|s| s.path == "C")
    }

    fn parse(node: Node<'_>, text: &'a str, previous_end: usize) -> Result<Self, ImportEditError> {
        let mut start = node;
        let mut block = ImportBlock::default();
        while let Some(comment) = start.prev_named_sibling() {
            if comment.kind() != "comment"
                || comment.start_byte() < previous_end
                || comment.end_position().row + 1 < start.start_position().row
            {
                break;
            }
            block.leading.push(comment_text(comment, text));
            start = comment;
        }
        block.leading.reverse();
        let mut grouped = false;
        for child in node.named_children(&mut node.walk()) {
            match child.kind() {
                "comment" => block.leading.push(comment_text(child, text)),
                "import_spec" => {
                    let mut spec = Spec::parse(child, text)?;
                    block.leading.append(&mut spec.leading);
                    spec.leading = std::mem::take(&mut block.leading);
                    block.specs.push(spec);
                }
                "import_spec_list" => {
                    grouped = true;
                    block.read_list(child, text)?;
                }
                _ => return Err(ImportEditError::UnsafeSyntaxTree),
            }
        }
        let (end, trailing) = declaration_tail(node, text);
        if let [spec] = &mut block.specs[..]
            && !grouped
        {
            spec.trailing = trailing;
        } else {
            block.trailing = trailing;
        }
        Ok(Self {
            bytes: start.start_byte()..end,
            grouped,
            block,
        })
    }
}

fn comment_text<'a>(node: Node<'_>, text: &'a str) -> &'a str {
    node_text(node, text).trim_end_matches('\r')
}

fn declaration_tail<'a>(mut node: Node<'_>, text: &'a str) -> (usize, Vec<&'a str>) {
    let mut comments = Vec::new();
    while let Some(next) = node.next_sibling() {
        if next.start_position().row != node.end_position().row {
            break;
        }
        match next.kind() {
            "comment" => comments.push(comment_text(next, text)),
            ";" => {}
            _ => break,
        }
        node = next;
    }
    let end = node.start_byte() + node_text(node, text).trim_end_matches('\r').len();
    (end, comments)
}

#[derive(Default)]
struct ImportBlock<'a> {
    leading: Vec<&'a str>,
    specs: Vec<Spec<'a>>,
    footer: Vec<&'a str>,
    trailing: Vec<&'a str>,
}

impl<'a> ImportBlock<'a> {
    fn read_list(&mut self, list: Node<'_>, text: &'a str) -> Result<(), ImportEditError> {
        let mut pending = Vec::new();
        let mut last_line = list.start_position().row;
        let mut gap = false;
        let mut seen_external = false;
        let mut in_local = false;
        for child in list.named_children(&mut list.walk()) {
            let line = child.start_position().row;
            gap |= line > last_line + 1;
            match child.kind() {
                "comment" => {
                    let comment = comment_text(child, text);
                    if line == last_line
                        && pending.is_empty()
                        && let Some(spec) = self.specs.last_mut()
                    {
                        spec.trailing.push(comment);
                    } else {
                        pending.push(comment);
                    }
                }
                "import_spec" => {
                    let mut spec = Spec::parse(child, text)?;
                    pending.append(&mut spec.leading);
                    spec.leading = std::mem::take(&mut pending);
                    in_local |= gap
                        && !self.specs.is_empty()
                        && (seen_external || spec.group == Group::Standard);
                    seen_external |= spec.group == Group::ThirdParty;
                    if in_local {
                        spec.group = Group::Local;
                    }
                    self.specs.push(spec);
                    gap = false;
                }
                _ => return Err(ImportEditError::UnsafeSyntaxTree),
            }
            last_line = child.end_position().row;
        }
        self.footer = pending;
        Ok(())
    }

    fn sort(&mut self) {
        self.specs
            .sort_by(|a, b| (a.group, &a.path, a.alias).cmp(&(b.group, &b.path, b.alias)));
    }

    fn render(&self, eol: &str, keep_group: bool) -> String {
        let mut lines: Vec<String> = self.leading.iter().map(|s| s.to_string()).collect();
        match self.specs.as_slice() {
            [] => {
                lines.extend(
                    self.footer
                        .iter()
                        .chain(&self.trailing)
                        .map(|s| s.to_string()),
                );
                return lines.join(eol);
            }
            [spec]
                if !keep_group
                    && self.leading.is_empty()
                    && spec.leading.is_empty()
                    && self.footer.is_empty() =>
            {
                lines.push(format!("import {}", spec.line(&self.trailing)));
                return lines.join(eol);
            }
            specs => {
                lines.push("import (".into());
                let mut previous = None;
                for spec in specs {
                    if previous.is_some_and(|group| group != spec.group) {
                        lines.push(String::new());
                    }
                    lines.extend(spec.leading.iter().map(|comment| format!("\t{comment}")));
                    lines.push(format!("\t{}", spec.line(&[])));
                    previous = Some(spec.group);
                }
                lines.extend(self.footer.iter().map(|comment| format!("\t{comment}")));
                lines.push(")".into());
            }
        }
        with_trailing_comments(lines.join(eol), self.trailing.iter().copied())
    }
}

struct Spec<'a> {
    path: String,
    alias: Option<&'a str>,
    literal: Cow<'a, str>,
    leading: Vec<&'a str>,
    trailing: Vec<&'a str>,
    group: Group,
}

impl<'a> Spec<'a> {
    fn new(path: String, alias: Option<&'a str>, literal: Cow<'a, str>) -> Self {
        let group = classify(&path, &[]);
        Self {
            path,
            alias,
            literal,
            leading: Vec::new(),
            trailing: Vec::new(),
            group,
        }
    }

    fn parse(node: Node<'_>, text: &'a str) -> Result<Self, ImportEditError> {
        let literal = node
            .child_by_field_name("path")
            .ok_or(ImportEditError::UnsafeSyntaxTree)?;
        let path = literal_path(literal, text)
            .filter(|p| valid_path(p))
            .ok_or(ImportEditError::InvalidImportPath)?;
        let alias = node.child_by_field_name("name").map(|n| node_text(n, text));
        let mut spec = Self::new(path, alias, node_text(literal, text).into());
        spec.leading.extend(
            node.named_children(&mut node.walk())
                .filter(|n| n.kind() == "comment")
                .map(|n| comment_text(n, text)),
        );
        Ok(spec)
    }

    fn binding(&self) -> &str {
        self.alias.unwrap_or_else(|| assumed_name(&self.path))
    }

    fn equivalent(&self, path: &str, alias: Option<&str>) -> bool {
        self.path == path
            && match (self.alias, alias) {
                (Some(name), None) | (None, Some(name)) => {
                    !matches!(name, "_" | ".") && name == assumed_name(path)
                }
                (a, b) => a == b,
            }
    }

    fn is_unused(&self, unused: &HashSet<&str>) -> bool {
        self.path != "C"
            && !matches!(self.alias, Some("_" | "."))
            && unused.contains(self.path.as_str())
    }

    fn line(&self, extra: &[&str]) -> String {
        let line = match self.alias {
            Some(name) => format!("{name} {}", self.literal),
            None => self.literal.to_string(),
        };
        with_trailing_comments(line, self.trailing.iter().chain(extra).copied())
    }
}

fn with_trailing_comments<'a>(
    mut line: String,
    comments: impl Iterator<Item = &'a str> + Clone,
) -> String {
    // A line comment would swallow the opening delimiter of a later block comment.
    for comment in comments
        .clone()
        .filter(|s| !s.starts_with("//"))
        .chain(comments.filter(|s| s.starts_with("//")))
    {
        line.push(' ');
        line.push_str(comment);
    }
    line
}

// Go's numeric escapes may spell individual UTF-8 bytes as well as Unicode scalars.
fn literal_path(node: Node<'_>, text: &str) -> Option<String> {
    let literal = node_text(node, text);
    if node.kind() == "raw_string_literal" || !literal.contains('\\') {
        return import_path(literal);
    }
    let mut rest = literal.strip_prefix('"')?.strip_suffix('"')?;
    let mut bytes = Vec::with_capacity(rest.len());
    while let Some((plain, escape)) = rest.split_once('\\') {
        bytes.extend_from_slice(plain.as_bytes());
        let (digits, radix, width) = match escape.as_bytes().first()? {
            b'x' => (&escape[1..], 16, 2),
            b'u' => (&escape[1..], 16, 4),
            b'U' => (&escape[1..], 16, 8),
            _ => (escape, 8, 3),
        };
        let (digits, tail) = digits.split_at_checked(width)?;
        let value = u32::from_str_radix(digits, radix).ok()?;
        if width <= 3 {
            bytes.push(u8::try_from(value).ok()?);
        } else {
            bytes.extend_from_slice(char::from_u32(value)?.encode_utf8(&mut [0; 4]).as_bytes());
        }
        rest = tail;
    }
    bytes.extend_from_slice(rest.as_bytes());
    String::from_utf8(bytes).ok()
}

fn valid_path(path: &str) -> bool {
    !path.split('/').any(str::is_empty)
        && !path.chars().any(|c| {
            c.is_control()
                || c.is_whitespace()
                || "!\"#$%&'()*,:;<=>?[\\]^\x60{|}\u{fffd}".contains(c)
        })
}

fn valid_alias(name: &str) -> bool {
    if name == "." {
        return true;
    }
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
        && !matches!(
            name,
            "break"
                | "case"
                | "chan"
                | "const"
                | "continue"
                | "default"
                | "defer"
                | "else"
                | "fallthrough"
                | "for"
                | "func"
                | "go"
                | "goto"
                | "if"
                | "import"
                | "interface"
                | "map"
                | "package"
                | "range"
                | "return"
                | "select"
                | "struct"
                | "switch"
                | "type"
                | "var"
        )
}

fn assumed_name(path: &str) -> &str {
    let mut parts = path.rsplit('/');
    let mut name = parts.next().unwrap_or(path);
    if name.strip_prefix('v').is_some_and(|n| {
        !n.is_empty() && n != "1" && !n.starts_with('0') && n.bytes().all(|c| c.is_ascii_digit())
    }) && let Some(parent) = parts.next()
    {
        name = parent;
    }
    name.strip_prefix("go-").unwrap_or(name)
}

fn redundant_aliases(header: &Header<'_>, unused: &HashSet<&str>) -> HashSet<String> {
    let mut bindings = HashMap::new();
    let mut conflicts = HashSet::new();
    let mut renamed = HashSet::new();
    let mut redundant = HashSet::new();
    for spec in header.declarations.iter().flat_map(|decl| {
        let cgo = decl.cgo();
        decl.block
            .specs
            .iter()
            .filter(move |spec| cgo || !spec.is_unused(unused))
    }) {
        if matches!(spec.alias, Some("_" | ".")) {
            continue;
        }
        let name = spec.binding();
        if bindings
            .insert(name, &spec.path)
            .is_some_and(|p| p != &spec.path)
        {
            conflicts.insert(name);
        }
        if spec.alias.is_some() {
            if name == assumed_name(&spec.path) {
                redundant.insert(spec.path.clone());
            } else {
                renamed.insert(&spec.path);
            }
        }
    }
    redundant.retain(|p| !renamed.contains(p) && !conflicts.contains(assumed_name(p)));
    redundant
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum Group {
    Standard,
    ThirdParty,
    Local,
}

fn classify(path: &str, local_prefixes: &[&str]) -> Group {
    if local_prefixes.iter().any(|prefix| {
        let prefix = prefix.trim_end_matches('/');
        !prefix.is_empty()
            && (path == prefix
                || path
                    .strip_prefix(prefix)
                    .is_some_and(|s| s.starts_with('/')))
    }) {
        Group::Local
    } else if path.split('/').next().is_some_and(|s| s.contains('.')) {
        Group::ThirdParty
    } else {
        Group::Standard
    }
}

fn insertion_group(path: &str, existing: &[Spec<'_>]) -> Group {
    let default = classify(path, &[]);
    // AddImport has no module prefixes. An existing paragraph with a shared path
    // prefix gives a new sibling the same placement in this document snapshot.
    existing
        .iter()
        .filter(|s| s.group == default || s.group == Group::Local)
        .map(|s| {
            let shared = path
                .split('/')
                .zip(s.path.split('/'))
                .take_while(|(a, b)| a == b)
                .count();
            (shared, std::cmp::Reverse(s.group))
        })
        .filter(|(shared, _)| *shared > 0)
        .max()
        .map_or(default, |(_, group)| group.0)
}

fn whitespace_after(text: &str, end: usize) -> Range<usize> {
    let rest = &text[end..];
    let gap = rest.len() - rest.trim_start_matches([' ', '\t', '\r', '\n']).len();
    let len = if gap == rest.len() {
        gap
    } else {
        rest[..gap].rfind('\n').map_or(gap, |i| i + 1)
    };
    end..end + len
}

fn replace(
    document: &DocumentSnapshot<'_>,
    mut bytes: Range<usize>,
    mut new: String,
    eol: &str,
) -> Option<TextEdit> {
    let text = document.text();
    if new.is_empty() {
        let line_start = text[..bytes.start].rfind('\n').map_or(0, |i| i + 1);
        if text[line_start..bytes.start].trim().is_empty() {
            bytes.start = line_start;
        }
        bytes.end = whitespace_after(text, bytes.end).end;
        if bytes.end == text.len() {
            let prefix = &text[..bytes.start];
            let space = prefix.trim_end_matches([' ', '\t', '\r', '\n']).len();
            if let Some(first_break) = prefix[space..].find('\n') {
                bytes.start = space + first_break + 1;
            }
        }
    } else if let Some(declaration) = text[bytes.clone()].strip_suffix(';') {
        if declaration == new {
            return None;
        }
        let rest = &text[bytes.end..];
        let next = rest.trim_start_matches([' ', '\t']);
        if !next.is_empty() && !next.starts_with(['\r', '\n']) {
            new.push_str(eol);
            bytes.end += rest.len() - next.len();
        }
    }
    (text[bytes.clone()] != new).then(|| TextEdit::new(document.byte_range(bytes), new))
}

#[cfg(test)]
mod tests {
    use lsp_types::{Position, Range, TextEdit};

    use crate::semantic::Engine;
    use crate::semantic::actions::{ImportEditError, ImportOperation, import_edits};
    use crate::test_support::TestProject;

    fn apply(
        text: &str,
        operation: ImportOperation<'_>,
    ) -> Result<(Vec<TextEdit>, String), ImportEditError> {
        let project = TestProject::new();
        let path = project.root.join("p.go");
        let mut engine = Engine::new(project.root.clone());
        engine.open(path.clone(), text.into(), 1).unwrap();
        let document = engine.snapshot().document(&path).unwrap().unwrap();
        let edits = import_edits(&document, operation)?;
        for pair in edits.windows(2) {
            assert!(
                pair[0].range.start < pair[1].range.start
                    && pair[0].range.end <= pair[1].range.start
            );
        }
        let mut result = text.to_owned();
        for edit in edits.iter().rev() {
            let start = document.offset(edit.range.start).unwrap();
            let end = document.offset(edit.range.end).unwrap();
            assert_eq!(document.byte_range(start..end), edit.range);
            result.replace_range(start..end, &edit.new_text);
        }
        Ok((edits, result))
    }

    fn add<'a>(path: &'a str, alias: Option<&'a str>) -> ImportOperation<'a> {
        ImportOperation::AddImport { path, alias }
    }

    fn check_add(before: &str, path: &str, alias: Option<&str>, after: &str) -> Vec<TextEdit> {
        let (edits, result) = apply(before, add(path, alias)).unwrap();
        assert_eq!(result, after);
        assert!(apply(after, add(path, alias)).unwrap().0.is_empty());
        edits
    }

    fn check_organize(
        before: &str,
        unused_paths: &[&str],
        local_prefixes: &[&str],
        after: &str,
    ) -> Vec<TextEdit> {
        let organize = ImportOperation::OrganizeImports {
            unused_paths,
            local_prefixes,
        };
        let (edits, result) = apply(before, organize).unwrap();
        assert_eq!(result, after);
        assert!(apply(after, organize).unwrap().0.is_empty());
        edits
    }

    #[test]
    fn adds_after_package_and_before_declaration_docs() {
        check_add("package p", "fmt", None, "package p\n\nimport \"fmt\"\n");
        check_add(
            "// License\npackage p // keep\n// F runs.\nfunc F() {}\n",
            "fmt",
            None,
            "// License\npackage p // keep\n\nimport \"fmt\"\n\n// F runs.\nfunc F() {}\n",
        );
        check_add(
            "package p /* one\n two */\n",
            "io",
            None,
            "package p /* one\n two */\n\nimport \"io\"\n",
        );
    }

    #[test]
    fn upgrades_single_import_and_moves_its_comments_with_it() {
        check_add(
            "package p\n\n// HTTP client.\nimport \"net/http\" // transport\n",
            "fmt",
            None,
            "package p\n\nimport (\n\t\"fmt\"\n\t// HTTP client.\n\t\"net/http\" // transport\n)\n",
        );
        check_add(
            "package p\nimport \"fmt\"\n",
            "example.org/log",
            Some("logger"),
            "package p\nimport (\n\t\"fmt\"\n\n\tlogger \"example.org/log\"\n)\n",
        );
    }

    #[test]
    fn inserts_into_all_three_existing_groups_by_path_segments() {
        let before = concat!(
            "package p\n\nimport (\n\t\"fmt\"\n\t\"os\"\n\n",
            "\t\"host.test/deps/x\"\n\n\t// Project package.\n\t\"host.test/project/z\" // Z\n)\n",
        );
        for (path, anchor, addition) in [
            ("net/http", "\t\"os\"", "\t\"net/http\"\n"),
            (
                "host.test/deps/a",
                "\t\"host.test/deps/x\"",
                "\t\"host.test/deps/a\"\n",
            ),
            (
                "another.test/x",
                "\t\"host.test/deps/x\"",
                "\t\"another.test/x\"\n",
            ),
            (
                "host.test/project/a",
                "\t// Project",
                "\t\"host.test/project/a\"\n",
            ),
        ] {
            check_add(
                before,
                path,
                None,
                &before.replace(anchor, &format!("{addition}{anchor}")),
            );
        }
    }

    #[test]
    fn adds_to_existing_parenthesized_declaration_without_organizing_other_imports() {
        check_add(
            "package p\nimport \"os\"\nimport ()\n",
            "io",
            None,
            "package p\nimport \"os\"\nimport (\n\t\"io\"\n)\n",
        );
    }

    #[test]
    fn equivalent_bindings_return_no_edits_and_explicit_aliases_stay_distinct() {
        for (import, path, alias) in [
            ("\"fmt\"", "fmt", None),
            ("fmt \"fmt\"", "fmt", None),
            (
                "\"host.test/go-client/v12\"",
                "host.test/go-client/v12",
                Some("client"),
            ),
            (
                "renamed \"host.test/client\"",
                "host.test/client",
                Some("renamed"),
            ),
            ("_ \"host.test/hooks\"", "host.test/hooks", Some("_")),
            (". \"host.test/dot\"", "host.test/dot", Some(".")),
        ] {
            let source = format!("package p\nimport {import}\n");
            assert!(check_add(&source, path, alias, &source).is_empty());
        }
        check_add(
            "package p\nimport _ \"fmt\"\n",
            "fmt",
            Some("f"),
            "package p\nimport (\n\t_ \"fmt\"\n\tf \"fmt\"\n)\n",
        );
    }

    #[test]
    fn organizes_multiple_declarations_and_preserves_comment_ownership() {
        check_organize(
            concat!(
                "package p\n\nimport (\n\t// Wire codec.\n\twire \"host.test/go-wire/v2\" // codec\n",
                "\t\"host.test/project/sub\"\n\t\"os\"\n\t\"fmt\" // formatter\n)\n",
                "import \"fmt\"\nimport _ \"host.test/hooks\"\nimport . \"host.test/dot\"\n",
                "import \"host.test/project2\"\nimport \"host.test/project\"\n\nvar _ = 1\n",
            ),
            &["os", "host.test/hooks", "host.test/dot"],
            &["host.test/project"],
            concat!(
                "package p\n\nimport (\n\t\"fmt\" // formatter\n\n",
                "\t. \"host.test/dot\"\n\t// Wire codec.\n\t\"host.test/go-wire/v2\" // codec\n",
                "\t_ \"host.test/hooks\"\n\t\"host.test/project2\"\n\n\t\"host.test/project\"\n",
                "\t\"host.test/project/sub\"\n)\n\nvar _ = 1\n",
            ),
        );
    }

    #[test]
    fn keeps_different_aliases_and_skips_conflicting_assumed_names() {
        let before = concat!(
            "package p\nimport (\n\tthing \"host.test/thing\"\n\ta \"host.test/thing\"\n",
            "\tsame \"one.test/same\"\n\t\"two.test/same\"\n\tlib \"host.test/lib/v3\"\n",
            "\t\"host.test/lib/v3\"\n\tv1 \"host.test/v1\"\n)\n",
        );
        check_organize(
            before,
            &[],
            &[],
            concat!(
                "package p\nimport (\n\t\"host.test/lib/v3\"\n\ta \"host.test/thing\"\n",
                "\tthing \"host.test/thing\"\n\t\"host.test/v1\"\n",
                "\tsame \"one.test/same\"\n\t\"two.test/same\"\n)\n",
            ),
        );
    }

    #[test]
    fn retains_all_comments_on_deduplicated_specs() {
        check_organize(
            "package p\nimport (\n\t// First\n\tf \"fmt\" /* one */\n\t// Second\n\tf \"fmt\" // two\n)\n",
            &[],
            &[],
            "package p\nimport (\n\t// First\n\t// Second\n\tf \"fmt\" /* one */ // two\n)\n",
        );
        check_organize(
            "package p\nimport (\n\t\"fmt\" // one\n\t\"fmt\" /* two\n\tmore */\n) // three\n",
            &[],
            &[],
            "package p\nimport \"fmt\" /* two\n\tmore */ // one // three\n",
        );
    }

    #[test]
    fn groups_exact_and_child_paths_for_multiple_local_prefixes() {
        check_organize(
            "package p\nimport (\n\t\"local/util\"\n\t\"app.test/mod2\"\n\t\"fmt\"\n\t\"app.test/mod\"\n)\n",
            &[],
            &["app.test/mod", "local/"],
            "package p\nimport (\n\t\"fmt\"\n\n\t\"app.test/mod2\"\n\n\t\"app.test/mod\"\n\t\"local/util\"\n)\n",
        );
        let grouped =
            "package p\nimport (\n\t\"fmt\"\n\n\t\"app.test/mod2\"\n\n\t\"local/util\"\n)\n";
        check_add(
            grouped,
            "local/app",
            None,
            &grouped.replace("\t\"local/util\"", "\t\"local/app\"\n\t\"local/util\""),
        );
    }

    #[test]
    fn cgo_preamble_and_declaration_are_outside_every_edit() {
        let cgo = "// #cgo CFLAGS: -std=c11\n/*\n#include <stdio.h>\n*/\nimport  \"C\" /* keep */";
        let before =
            format!("package p\n\nimport \"os\"\n\n{cgo}\n\nimport \"fmt\"\n\nvar _ C.int\n");
        let after =
            format!("package p\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\n{cgo}\n\nvar _ C.int\n");
        let edits = check_organize(&before, &["C"], &[], &after);
        assert_eq!(edits.len(), 2);
        assert!(edits[0].range.end < Position::new(4, 0));
        assert!(edits[1].range.start > Position::new(8, 0));
        let only_cgo = format!("package p\n\n{cgo}\n");
        check_add(
            &only_cgo,
            "fmt",
            None,
            &format!("{only_cgo}\nimport \"fmt\"\n"),
        );
        assert!(check_organize(&only_cgo, &["C"], &[], &only_cgo).is_empty());
    }

    #[test]
    fn adds_cgo_separately_and_leaves_parenthesized_cgo_in_place() {
        check_add(
            "package p\nimport \"fmt\"\n",
            "C",
            None,
            "package p\nimport \"fmt\"\n\nimport \"C\"\n",
        );
        let before = "package p\n\nimport (\n\t// #include <stdlib.h>\n\t\"C\"\n\t\"os\"\n)\n";
        assert!(check_organize(before, &["C"], &[], before).is_empty());
        check_add(before, "fmt", None, &format!("{before}\nimport \"fmt\"\n"));
    }

    #[test]
    fn cleans_up_empty_declarations_and_collapses_one_surviving_spec() {
        check_organize(
            "package p\n\nimport ()\n\nfunc f() {}\n",
            &[],
            &[],
            "package p\n\nfunc f() {}\n",
        );
        check_organize(
            "package p\n\nimport \"os\"\n\nimport ()\n",
            &["os"],
            &[],
            "package p\n",
        );
        check_organize(
            "package p\nimport (\n\t\"fmt\" // output\n\t\"os\"\n)\n",
            &["os"],
            &[],
            "package p\nimport \"fmt\" // output\n",
        );
        let canonical =
            "package p\n\nimport (\n\t\"fmt\"\n\n\t\"external.test/x\"\n\n\t\"here.test/app\"\n)\n";
        assert!(check_organize(canonical, &[], &["here.test/app"], canonical).is_empty());
    }

    #[test]
    fn collapsing_before_an_inline_declaration_keeps_code_after_a_line_comment() {
        check_organize(
            "package p; import (\n\t\"fmt\" // output\n); var x = fmt.Sprint\n",
            &[],
            &[],
            "package p; import \"fmt\" // output\nvar x = fmt.Sprint\n",
        );
        check_organize(
            "package p; import \"os\"; var x int\n",
            &["os"],
            &[],
            "package p; var x int\n",
        );
    }

    #[test]
    fn preserves_standalone_comments_at_declaration_boundaries() {
        check_organize(
            "package p\n\nimport \"os\"\n\n// File note.\n\nimport \"fmt\"\n\nfunc f() {}\n",
            &[],
            &[],
            "package p\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\n// File note.\n\nfunc f() {}\n",
        );
        check_organize(
            "package p\n\nimport (\n\t// File note.\n)\n",
            &[],
            &[],
            "package p\n\n// File note.\n",
        );
        let documented = "package p\n// IO dependencies.\nimport (\n\t\"fmt\"\n)\n";
        assert!(check_organize(documented, &[], &[], documented).is_empty());
    }

    #[test]
    fn preserves_crlf_and_measures_both_edit_endpoints_in_utf16() {
        let edits = check_add(
            "package p // 😀\r\n\r\n// F.\r\nfunc F() {}\r\n",
            "fmt",
            None,
            "package p // 😀\r\n\r\nimport \"fmt\"\r\n\r\n// F.\r\nfunc F() {}\r\n",
        );
        assert_eq!(edits[0].range.start, Position::new(0, 15));
        let before = "package 𐐀; import \"os\" // 🧪";
        let after = "package 𐐀; import (\r\n\t\"fmt\"\r\n\t\"os\" // 🧪\r\n)";
        let edits = check_add(
            &format!("{before}\r\n"),
            "fmt",
            None,
            &format!("{after}\r\n"),
        );
        assert_eq!(
            edits[0].range,
            Range::new(
                Position::new(0, 12),
                Position::new(0, before.encode_utf16().count() as u32)
            )
        );
    }

    #[test]
    fn sorts_by_decoded_paths_and_preserves_raw_and_escaped_spellings() {
        let before = "package p\nimport (\n\t\x60os\x60\n\t\"\\x66\\155\\u0074\"\n)\n";
        assert!(check_add(before, "fmt", None, before).is_empty());
        check_organize(
            before,
            &[],
            &[],
            "package p\nimport (\n\t\"\\x66\\155\\u0074\"\n\t\x60os\x60\n)\n",
        );
        let cgo = "package p\n// #include <stdio.h>\nimport \"\\x43\"\n";
        assert!(check_organize(cgo, &["C"], &[], cgo).is_empty());
        let escaped = "package p\nimport \"host.test/\\x63afe\\xc3\\xa9\"\n";
        assert!(check_add(escaped, "host.test/cafeé", None, escaped).is_empty());
    }

    #[test]
    fn rejects_missing_or_unsafe_package_and_import_regions() {
        for source in ["", "// No package\n", "import \"fmt\"\n"] {
            assert_eq!(
                apply(source, add("fmt", None)),
                Err(ImportEditError::MissingPackageClause),
                "{source:?}"
            );
        }
        for source in [
            "package\n",
            "package @\n",
            "package p import \"fmt\"\n",
            "package\nimport \"fmt\"\n",
            "package p\nimport (\n\t\"fmt\"\n",
            "package p\nimport @\n",
            "package p\nimport \"fmt\"\nimport (\n\tbad\n)\n",
            "package p\nimport \"unterminated\n",
            "package p\nvar x int\nimport \"fmt\"\n",
        ] {
            for operation in [
                add("fmt", None),
                ImportOperation::OrganizeImports {
                    unused_paths: &[],
                    local_prefixes: &[],
                },
            ] {
                assert_eq!(
                    apply(source, operation),
                    Err(ImportEditError::UnsafeSyntaxTree),
                    "{source:?}"
                );
            }
        }
    }

    #[test]
    fn incomplete_function_bodies_allow_import_completion() {
        check_add(
            "package p\n\nfunc f() { value. }\n",
            "fmt",
            None,
            "package p\n\nimport \"fmt\"\n\nfunc f() { value. }\n",
        );
    }

    #[test]
    fn rejects_invalid_paths_and_aliases_before_emitting_edits() {
        for path in ["", "x y", "x\\y", "x\nvar", "a\"b", "x//y", "/absolute"] {
            assert_eq!(
                apply("package p", add(path, None)),
                Err(ImportEditError::InvalidImportPath)
            );
        }
        for alias in ["", "7f", "import", "log/path", "x\ny"] {
            assert_eq!(
                apply("package p", add("fmt", Some(alias))),
                Err(ImportEditError::InvalidImportAlias)
            );
        }
        assert_eq!(
            apply("package p\nimport \"bad path\"\n", add("fmt", None)),
            Err(ImportEditError::InvalidImportPath)
        );
    }
}
