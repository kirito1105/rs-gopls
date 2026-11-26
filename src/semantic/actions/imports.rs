use std::fmt;
use std::ops::Range;
use std::sync::LazyLock;

use lsp_types::TextEdit;
use regex::Regex;
use tree_sitter::Node;

use super::super::snapshot::DocumentSnapshot;
use super::super::source::{decoded_import_path, import_specs, node_text};

pub(crate) enum ImportOperation<'a> {
    Add {
        path: &'a str,
        alias: Option<&'a str>,
    },
    Organize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImportEditError {
    MissingPackageClause,
    MalformedImportBlock,
    InvalidImportPath,
    InvalidImportAlias,
    ConflictingBinding {
        path: String,
        alias: Option<String>,
        existing_path: String,
        existing_alias: Option<String>,
    },
    UnsupportedOperation(&'static str),
}

impl fmt::Display for ImportEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPackageClause => f.write_str("missing or invalid Go package clause"),
            Self::MalformedImportBlock => {
                f.write_str("cannot edit a malformed Go import declaration")
            }
            Self::InvalidImportPath => f.write_str("invalid Go import path"),
            Self::InvalidImportAlias => f.write_str("invalid Go import alias"),
            Self::ConflictingBinding {
                path,
                alias,
                existing_path,
                existing_alias,
            } => write!(
                f,
                "import {path:?} with alias {alias:?} conflicts with {existing_path:?} with alias {existing_alias:?}",
            ),
            Self::UnsupportedOperation(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ImportEditError {}

struct ImportSpec<'a> {
    path: String,
    alias: Option<&'a str>,
    start: usize,
    end: usize,
    indent: Option<&'a str>,
}

struct ImportBlock<'tree, 'text> {
    declaration: Node<'tree>,
    close: Option<Node<'tree>>,
    specs: Vec<ImportSpec<'text>>,
}

/// Builds an edit against the supplied snapshot. Each addition needs at most
/// one edit, so the returned ranges are ordered and cannot overlap.
pub(crate) fn import_edits(
    document: &DocumentSnapshot<'_>,
    operation: ImportOperation<'_>,
) -> Result<Vec<TextEdit>, ImportEditError> {
    let ImportOperation::Add { path, alias } = operation else {
        return Err(ImportEditError::UnsupportedOperation(
            "organize imports is reserved for Track D",
        ));
    };
    if !valid_path(path) {
        return Err(ImportEditError::InvalidImportPath);
    }
    if let Some(alias) = alias
        && (path == "C" || alias == "init" || (alias != "." && !valid_identifier(alias)))
    {
        return Err(ImportEditError::InvalidImportAlias);
    }

    let text = document.text();
    let (package, blocks) = import_region(document.root_node(), text)?;
    let specs = || blocks.iter().flat_map(|block| &block.specs);
    if specs().any(|spec| spec.path == path && spec.alias == alias) {
        return Ok(Vec::new());
    }
    if let Some(existing) = specs().find(|spec| binding_conflicts(spec, path, alias)) {
        return Err(ImportEditError::ConflictingBinding {
            path: path.to_owned(),
            alias: alias.map(str::to_owned),
            existing_path: existing.path.clone(),
            existing_alias: existing.alias.map(str::to_owned),
        });
    }

    let eol = if text
        .find('\n')
        .is_some_and(|index| text[..index].ends_with('\r'))
    {
        "\r\n"
    } else {
        "\n"
    };
    // The path was validated before rendering, including the literal delimiters.
    let spec = match alias {
        Some(alias) => format!("{alias} \"{path}\""),
        None => format!("\"{path}\""),
    };
    let key = import_key(path, alias);
    let block = blocks
        .iter()
        .filter(|block| path != "C" && block.specs.iter().all(|spec| spec.path != "C"))
        .min_by_key(|block| {
            (
                block.close.is_none(),
                !block
                    .specs
                    .iter()
                    .any(|entry| is_external(&entry.path) == key.0),
            )
        });
    let (bytes, new_text) = match block {
        Some(block) if block.close.is_some() => insert_in_block(block, text, eol, &spec, key),
        Some(block) => group_single(block, text, eol, &spec, key),
        None => insert_declaration(
            blocks.last().map_or(package, |block| block.declaration),
            text,
            eol,
            &spec,
        ),
    };
    Ok(vec![TextEdit::new(document.byte_range(bytes), new_text)])
}

fn valid_path(path: &str) -> bool {
    const FORBIDDEN: &str = concat!(r##"!"#$%&'()*,:;<=>?[\]^{|}"##, "\x60\u{fffd}");
    static GRAPHIC: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\A[\p{L}\p{M}\p{N}\p{P}\p{S}]+\z").unwrap());
    GRAPHIC.is_match(path) && !path.contains(|ch| FORBIDDEN.contains(ch))
}

fn valid_identifier(name: &str) -> bool {
    static IDENTIFIER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\A[_\p{L}][_\p{L}\p{Nd}]*\z").unwrap());
    IDENTIFIER.is_match(name)
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

fn binding_conflicts(existing: &ImportSpec<'_>, path: &str, alias: Option<&str>) -> bool {
    if existing.alias == Some("_") || alias == Some("_") {
        return false;
    }
    if existing.path == path {
        return existing.alias != alias;
    }
    // Default package names require package resolution, which the AST edit
    // contract leaves to the caller. Explicit named aliases are known here.
    alias.is_some_and(|alias| alias != "." && existing.alias == Some(alias))
}

fn import_region<'tree, 'text>(
    root: Node<'tree>,
    text: &'text str,
) -> Result<(Node<'tree>, Vec<ImportBlock<'tree, 'text>>), ImportEditError> {
    let mut cursor = root.walk();
    let mut nodes = root
        .named_children(&mut cursor)
        .filter(|node| node.kind() != "comment");
    let package = nodes
        .next()
        .filter(|node| node.kind() == "package_clause" && !node.has_error())
        .filter(|node| {
            node.named_children(&mut node.walk())
                .find(|child| child.kind() == "package_identifier")
                .is_some_and(|name| {
                    let name = node_text(name, text);
                    name != "_" && valid_identifier(name)
                })
        })
        .ok_or(ImportEditError::MissingPackageClause)?;
    let mut blocks = Vec::new();
    let mut imports_allowed = true;
    for node in nodes {
        match node.kind() {
            "import_declaration" if imports_allowed => blocks.push(parse_block(node, text)?),
            "import_declaration" => return Err(ImportEditError::MalformedImportBlock),
            // An unclosed declaration may be recovered as a top-level ERROR
            // wrapping an anonymous import token instead of an import node.
            "ERROR" if error_contains_import(node) => {
                return Err(ImportEditError::MalformedImportBlock);
            }
            _ => imports_allowed = false,
        }
    }
    Ok((package, blocks))
}

fn error_contains_import(node: Node<'_>) -> bool {
    node.children(&mut node.walk())
        .any(|child| match child.kind() {
            "import" | "import_declaration" => true,
            "ERROR" => error_contains_import(child),
            _ => false,
        })
}

fn parse_block<'tree, 'text>(
    declaration: Node<'tree>,
    text: &'text str,
) -> Result<ImportBlock<'tree, 'text>, ImportEditError> {
    let malformed = ImportEditError::MalformedImportBlock;
    if declaration.has_error() {
        return Err(malformed);
    }
    let list = declaration
        .named_children(&mut declaration.walk())
        .find(|child| child.kind() == "import_spec_list");
    let close = if let Some(list) = list {
        Some(
            list.children(&mut list.walk())
                .find(|node| node.kind() == ")" && !node.is_missing())
                .ok_or(malformed.clone())?,
        )
    } else {
        None
    };

    let mut specs = Vec::new();
    let mut previous_end = list
        .and_then(|list| list.child(0))
        .map_or(declaration.start_byte(), |open| trailing_end(open, text));
    for node in import_specs(declaration) {
        let literal = node.child_by_field_name("path").ok_or(malformed.clone())?;
        let path = decoded_import_path(node_text(literal, text))
            .filter(|path| valid_path(path))
            .ok_or(malformed.clone())?;
        let alias = node
            .child_by_field_name("name")
            .map(|node| node_text(node, text));
        let start = if list.is_some() {
            leading_start(node, previous_end, text)
        } else {
            declaration.named_child(0).unwrap_or(node).start_byte()
        };
        let end = trailing_end(if list.is_some() { node } else { declaration }, text);
        previous_end = end;
        let indent = line_indent(text, node.start_byte()).or_else(|| line_indent(text, start));
        specs.push(ImportSpec {
            path,
            alias,
            start,
            end,
            indent,
        });
    }
    if close.is_none() && specs.len() != 1 {
        return Err(malformed);
    }
    Ok(ImportBlock {
        declaration,
        close,
        specs,
    })
}

fn leading_start(node: Node<'_>, previous_end: usize, text: &str) -> usize {
    let mut start = node.start_byte();
    let mut sibling = node.prev_named_sibling();
    while let Some(comment) = sibling.filter(|node| node.kind() == "comment") {
        if comment.start_byte() < previous_end || has_blank_line(&text[comment.end_byte()..start]) {
            break;
        }
        start = comment.start_byte();
        sibling = comment.prev_named_sibling();
    }
    start
}

fn trailing_end(node: Node<'_>, text: &str) -> usize {
    let mut end = node.end_byte();
    let mut sibling = node.next_named_sibling();
    while let Some(comment) = sibling.filter(|node| node.kind() == "comment") {
        let gap = text[end..comment.start_byte()].trim_matches([' ', '\t']);
        if !(gap.is_empty() || (gap == ";" && node_text(comment, text).starts_with("//"))) {
            break;
        }
        end = comment.end_byte();
        if text[..end].ends_with('\r') && text.as_bytes().get(end) == Some(&b'\n') {
            end -= 1;
        }
        sibling = comment.next_named_sibling();
    }
    end
}

fn has_blank_line(gap: &str) -> bool {
    gap.split_inclusive('\n')
        .skip(1)
        .any(|line| line.ends_with('\n') && line.trim().is_empty())
}

fn is_external(path: &str) -> bool {
    path.contains('.')
}

type ImportKey<'a> = (bool, &'a str, Option<&'a str>);

fn import_key<'a>(path: &'a str, alias: Option<&'a str>) -> ImportKey<'a> {
    (is_external(path), path, alias)
}

impl ImportSpec<'_> {
    fn key(&self) -> ImportKey<'_> {
        import_key(&self.path, self.alias)
    }
}

fn insert_declaration(node: Node<'_>, text: &str, eol: &str, spec: &str) -> (Range<usize>, String) {
    let mut at = trailing_end(node, text);
    let rest = text[at..].trim_start_matches([' ', '\t']);
    if let Some(rest) = rest.strip_prefix(';') {
        at = text.len() - rest.len();
    }
    let rest = &text[at..];
    let next = rest.trim_start_matches([' ', '\t', '\r', '\n']);
    let gap = &rest[..rest.len() - next.len()];
    let newlines = gap.bytes().filter(|byte| *byte == b'\n').take(2).count();
    let (end, needed) = if next.is_empty() {
        (text.len(), 1)
    } else if newlines == 0 {
        (text.len() - next.len(), 2)
    } else {
        // Keep the original line's trailing spaces on that line.
        at += rest.len() - rest.trim_start_matches([' ', '\t']).len();
        (at, 2 - newlines)
    };
    (
        at..end,
        format!("{eol}{eol}import {spec}{}", eol.repeat(needed)),
    )
}

fn group_single(
    block: &ImportBlock<'_, '_>,
    text: &str,
    eol: &str,
    spec: &str,
    key: ImportKey<'_>,
) -> (Range<usize>, String) {
    let original = &block.specs[0];
    let old_spec = &text[original.start..original.end];
    let (first, second) = if key < original.key() {
        (spec, old_spec)
    } else {
        (old_spec, spec)
    };
    let gap = if key.0 != original.key().0 { eol } else { "" };
    let start = block.declaration.start_byte() + "import".len();
    (
        start..original.end,
        format!(" ({eol}\t{first}{eol}{gap}\t{second}{eol})"),
    )
}

fn insert_in_block(
    block: &ImportBlock<'_, '_>,
    text: &str,
    eol: &str,
    spec: &str,
    key: ImportKey<'_>,
) -> (Range<usize>, String) {
    let close = block.close.expect("parenthesized import block");
    let indent = block
        .specs
        .iter()
        .filter_map(|entry| entry.indent)
        .find(|indent| !indent.is_empty())
        .unwrap_or("\t");
    let group = block
        .specs
        .chunk_by(|left, right| !has_blank_line(&text[left.end..right.start]))
        .find(|group| group.iter().any(|entry| entry.key().0 == key.0));
    if let Some(group) = group {
        if let Some(after) = group.iter().find(|entry| entry.key() > key) {
            return insert_before(after.start, text, eol, indent, spec, false);
        }
        return insert_after(group.last().unwrap().end, text, eol, indent, spec, false);
    }
    match (block.specs.first(), block.specs.last()) {
        (Some(first), _) if !key.0 => insert_before(first.start, text, eol, indent, spec, true),
        (_, Some(last))
            if let Some(close_indent) = line_indent(text, close.start_byte())
                && text[last.end..close.start_byte()].trim().is_empty()
                && has_blank_line(&text[last.end..close.start_byte()]) =>
        {
            let at = close.start_byte() - close_indent.len();
            (at..at, format!("{indent}{spec}{eol}"))
        }
        (_, Some(last)) => insert_after(last.end, text, eol, indent, spec, true),
        _ => {
            let (at, prefix) = line_indent(text, close.start_byte())
                .map_or((close.start_byte(), eol), |indent| {
                    (close.start_byte() - indent.len(), "")
                });
            (at..at, format!("{prefix}{indent}{spec}{eol}"))
        }
    }
}

fn line_indent(text: &str, at: usize) -> Option<&str> {
    let line_start = text[..at].rfind('\n').map_or(0, |index| index + 1);
    let indent = &text[line_start..at];
    indent
        .bytes()
        .all(|byte| matches!(byte, b' ' | b'\t'))
        .then_some(indent)
}

fn insert_before(
    at: usize,
    text: &str,
    eol: &str,
    indent: &str,
    spec: &str,
    separate: bool,
) -> (Range<usize>, String) {
    let gap = if separate { eol } else { "" };
    if let Some(previous_indent) = line_indent(text, at) {
        let start = at - previous_indent.len();
        (start..start, format!("{indent}{spec}{eol}{gap}"))
    } else {
        (at..at, format!("{eol}{indent}{spec}{eol}{gap}{indent}"))
    }
}

fn insert_after(
    mut at: usize,
    text: &str,
    eol: &str,
    indent: &str,
    spec: &str,
    separate: bool,
) -> (Range<usize>, String) {
    let rest = text[at..].trim_start_matches([' ', '\t']);
    if rest.starts_with(['\r', '\n']) {
        at = text.len() - rest.len();
    }
    let gap = if separate { eol } else { "" };
    let suffix = if rest.starts_with(')') { eol } else { "" };
    (at..at, format!("{eol}{gap}{indent}{spec}{suffix}"))
}

#[cfg(test)]
mod tests {
    use lsp_types::Position;

    use super::*;
    use crate::semantic::Engine;
    use crate::test_support::TestProject;

    fn with_document<T>(text: &str, f: impl FnOnce(&DocumentSnapshot<'_>) -> T) -> T {
        let project = TestProject::new();
        let path = project.root.join("sample.go");
        let mut engine = Engine::new(project.root.clone());
        engine.open(path.clone(), text.into(), 1).unwrap();
        let mut snapshot = engine.snapshot();
        let document = snapshot.document(&path).unwrap().unwrap();
        f(&document)
    }

    fn add(text: &str, path: &str, alias: Option<&str>) -> Result<Vec<TextEdit>, ImportEditError> {
        with_document(text, |doc| {
            import_edits(doc, ImportOperation::Add { path, alias })
        })
    }

    fn byte_offset(text: &str, position: Position) -> usize {
        let start = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(byte, _)| byte + 1))
            .nth(position.line as usize)
            .expect("valid line");
        let line = text[start..]
            .split('\n')
            .next()
            .unwrap()
            .trim_end_matches('\r');
        let mut column = 0;
        for (byte, ch) in line.char_indices() {
            if column == position.character {
                return start + byte;
            }
            column += ch.len_utf16() as u32;
        }
        assert_eq!(column, position.character, "valid UTF-16 boundary");
        start + line.len()
    }

    fn apply(text: &str, edits: &[TextEdit]) -> String {
        let mut output = String::new();
        let mut from = 0;
        for edit in edits {
            let start = byte_offset(text, edit.range.start);
            let end = byte_offset(text, edit.range.end);
            assert!(from <= start && start <= end, "ordered, disjoint edits");
            output.push_str(&text[from..start]);
            output.push_str(&edit.new_text);
            from = end;
        }
        output.push_str(&text[from..]);
        output
    }

    fn assert_added(text: &str, path: &str, alias: Option<&str>, expected: &str) {
        let edits = add(text, path, alias).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(apply(text, &edits), expected);
        assert_eq!(add(expected, path, alias).unwrap(), []);
    }

    #[test]
    fn needs_a_valid_package_before_any_declarations() {
        for text in [
            "",
            "// no package\n",
            "var x int\npackage p\n",
            "package _\n",
            "package\n",
        ] {
            assert_eq!(
                add(text, "fmt", None),
                Err(ImportEditError::MissingPackageClause),
                "{text:?}"
            );
        }
    }

    #[test]
    fn inserts_first_import_with_a_single_trailing_newline_at_eof() {
        assert_added(
            "package p",
            "io",
            Some("io"),
            "package p\n\nimport io \"io\"\n",
        );
        assert_added(
            "package p\n",
            "io",
            Some("名٣"),
            "package p\n\nimport 名٣ \"io\"\n",
        );
        for eol in ["\n", "\r\n"] {
            for suffix in [eol.to_owned(), eol.repeat(2)] {
                let text = format!("package p{suffix}");
                let expected = format!("package p{eol}{eol}import io \"io\"{eol}");
                assert_added(&text, "io", Some("io"), &expected);
            }
        }
    }

    #[test]
    fn separates_package_and_declaration_comments_in_lf_and_crlf() {
        let text =
            "package /* 🧠猫 */ p // trailing 🦀\n// Doc belongs to the function.\nfunc F() {}\n";
        let expected = "package /* 🧠猫 */ p // trailing 🦀\n\nimport \"fmt\"\n\n// Doc belongs to the function.\nfunc F() {}\n";
        assert_added(text, "fmt", None, expected);
        assert_added(
            &text.replace('\n', "\r\n"),
            "fmt",
            None,
            &expected.replace('\n', "\r\n"),
        );
        assert_added(
            "package p; /* decl docs */ var a int\n",
            "fmt",
            None,
            "package p;\n\nimport \"fmt\"\n\n/* decl docs */ var a int\n",
        );
    }

    #[test]
    fn groups_single_import_with_its_exact_alias_literal_and_comments() {
        let text = concat!(
            "package p\n\n// import declaration docs\n",
            "import /* 🐱 lead */ alias /* keep */ \x60example.test/a\x60 // 尾 🦀\n",
            "\nfunc f() {}\n",
        );
        let expected = concat!(
            "package p\n\n// import declaration docs\n",
            "import (\n\t\"fmt\"\n\n\t/* 🐱 lead */ alias /* keep */ \x60example.test/a\x60 // 尾 🦀\n)\n",
            "\nfunc f() {}\n",
        );
        assert_added(text, "fmt", None, expected);
        assert_added(
            &text.replace('\n', "\r\n"),
            "fmt",
            None,
            &expected.replace('\n', "\r\n"),
        );
        assert_added(
            "package p\nimport /* immediately after import */\n\n\"io\"\n",
            "fmt",
            None,
            "package p\nimport (\n\t\"fmt\"\n\t/* immediately after import */\n\n\"io\"\n)\n",
        );
    }

    #[test]
    fn preserves_the_blank_import_when_adding_a_named_binding_for_its_path() {
        assert_added(
            "package p\nimport _ \"fmt\" // side effects\n",
            "fmt",
            Some("printer"),
            "package p\nimport (\n\t_ \"fmt\" // side effects\n\tprinter \"fmt\"\n)\n",
        );
        assert_added(
            "package p\nimport _ \"fmt\"\n",
            "fmt",
            None,
            "package p\nimport (\n\t\"fmt\"\n\t_ \"fmt\"\n)\n",
        );
    }

    #[test]
    fn preserves_import_groups_and_inserts_before_the_comments_for_the_next_spec() {
        let text = concat!(
            "package p\nimport (\n",
            "    \"bytes\" // existing stdlib import\n    // Relative docs\n    _ \"os\"\n",
            "\n    \"example.test/b\"\n    // belongs to the dot import 😀\n    . \"example.test/z\"\n",
            "\n    \"local/module\"\n)\n",
        );
        let expected = text.replace("    // Relative", "    \"fmt\"\n    // Relative");
        assert_added(text, "fmt", None, &expected);
        let expected = text.replace(
            "    // belongs",
            "    client \"example.test/c\"\n    // belongs",
        );
        assert_added(text, "example.test/c", Some("client"), &expected);
        assert_added(
            "package p\nimport ( // group header\n\t// belongs to os\n\t\"os\"\n)\n",
            "fmt",
            None,
            "package p\nimport ( // group header\n\t\"fmt\"\n\t// belongs to os\n\t\"os\"\n)\n",
        );
    }

    #[test]
    fn creates_a_missing_group_without_moving_existing_specs() {
        assert_added(
            "package p\nimport (\n\t// belongs to module\n\t\"example.test/module\"\n)\n",
            "fmt",
            None,
            "package p\nimport (\n\t\"fmt\"\n\n\t// belongs to module\n\t\"example.test/module\"\n)\n",
        );
        assert_added(
            "package p\nimport (\n  \"fmt\" // keep here\n)\n",
            "example.test/module",
            None,
            "package p\nimport (\n  \"fmt\" // keep here\n\n  \"example.test/module\"\n)\n",
        );
        assert_added(
            "package p\r\nimport (\r\n\t\"fmt\"\r\n\r\n)\r\n",
            "example.test/module",
            None,
            "package p\r\nimport (\r\n\t\"fmt\"\r\n\r\n\t\"example.test/module\"\r\n)\r\n",
        );
    }

    #[test]
    fn chooses_the_first_matching_block_and_preserves_cgo_preamble() {
        let text = concat!(
            "package p\n\n/*\n#include <stdio.h>\n*/\nimport \"C\" // cgo\n",
            "import ()\n",
            "import (\n\t\"example.test/a\"\n)\n",
            "import (\n  \"io\"\n)\n",
            "import (\n    \"os\"\n)\n",
        );
        let expected = text.replace("  \"io\"", "  \"fmt\"\n  \"io\"");
        assert_added(text, "fmt", None, &expected);

        let cgo_only = "package p\n\n// preamble\nimport \"C\" // cgo\n// F doc\nfunc F() {}\n";
        let expected = "package p\n\n// preamble\nimport \"C\" // cgo\n\nimport \"fmt\"\n\n// F doc\nfunc F() {}\n";
        assert_added(cgo_only, "fmt", None, expected);
    }

    #[test]
    fn handles_compact_and_empty_import_blocks() {
        assert_added(
            "package p\nimport ()\n",
            "fmt",
            None,
            "package p\nimport (\n\t\"fmt\"\n)\n",
        );
        assert_added(
            "package p\nimport (\"bytes\"; /* keep with os */ \"os\")\n",
            "fmt",
            None,
            "package p\nimport (\"bytes\"; \n\t\"fmt\"\n\t/* keep with os */ \"os\")\n",
        );
        assert_added(
            "package p; import \"os\"; var x int\n",
            "fmt",
            None,
            "package p; import (\n\t\"fmt\"\n\t\"os\"\n); var x int\n",
        );
    }

    #[test]
    fn detects_duplicates_by_go_decoded_path_and_explicit_alias() {
        let text = concat!(
            "package p\n",
            "import (\n\t_ \"\\x66mt\"\n\t. \"\\143ontext\"\n\talias \x60io\x60\n\t\"net/\\u0068ttp\"\n)\n",
        );
        for (path, alias) in [
            ("fmt", Some("_")),
            ("context", Some(".")),
            ("io", Some("alias")),
            ("net/http", None),
        ] {
            assert_eq!(add(text, path, alias).unwrap(), []);
        }
        assert!(matches!(
            add(text, "context", None),
            Err(ImportEditError::ConflictingBinding { .. })
        ));
        assert!(matches!(
            add(text, "io", None),
            Err(ImportEditError::ConflictingBinding { .. })
        ));
        assert!(matches!(
            add(text, "os", Some("alias")),
            Err(ImportEditError::ConflictingBinding { .. })
        ));
    }

    #[test]
    fn rejects_malformed_import_declarations_and_invalid_add_requests() {
        for text in [
            "package p\nimport (\n\"fmt\"\n",
            "package p\nimport (\n\"fmt\"\nfunc f() {}\n",
            "package p\nimport (\n\"io\"\n???\n)\n",
            "package p\nimport \"\\invalid\"\n",
            "package p\nimport ",
            "package p\nimport \"",
            "package p\nfunc f() {}\nimport \"io\"\n",
        ] {
            assert_eq!(
                add(text, "fmt", None),
                Err(ImportEditError::MalformedImportBlock),
                "{text:?}"
            );
        }
        for path in [
            "",
            "two words",
            "bad\"path",
            "bad\npath",
            "bad\\path",
            "bad\x60path",
            "bad\u{200d}path",
        ] {
            assert_eq!(
                add("package p\n", path, None),
                Err(ImportEditError::InvalidImportPath)
            );
        }
        for alias in [
            "",
            "2x",
            "two names",
            "var",
            "init",
            "bad-name",
            "x½",
            "x\u{0345}",
        ] {
            assert_eq!(
                add("package p\n", "fmt", Some(alias)),
                Err(ImportEditError::InvalidImportAlias)
            );
        }
        assert_eq!(
            with_document("package p\n", |doc| import_edits(
                doc,
                ImportOperation::Organize
            )),
            Err(ImportEditError::UnsupportedOperation(
                "organize imports is reserved for Track D"
            )),
        );
    }

    #[test]
    fn utf16_import_edit_and_main_completion_edit_apply_to_the_same_snapshot() {
        let text = "package p; import /* 🦀猫 */ \"io\"; var x = \"😀\" + fmt.Pr";
        with_document(text, |document| {
            let mut edits = import_edits(
                document,
                ImportOperation::Add {
                    path: "fmt",
                    alias: None,
                },
            )
            .unwrap();
            let start = text.find("Pr").unwrap();
            let main = TextEdit::new(document.byte_range(start..text.len()), "Print".into());
            assert!(edits[0].range.end.character < main.range.start.character);
            edits.push(main);
            assert_eq!(
                apply(text, &edits),
                "package p; import (\n\t\"fmt\"\n\t/* 🦀猫 */ \"io\"\n); var x = \"😀\" + fmt.Print",
            );
        });
    }
}
