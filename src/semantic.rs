use std::cell::OnceCell;
use std::collections::{HashMap, hash_map::Entry};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use lsp_types::{Position, Range, TextDocumentContentChangeEvent};
use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

use crate::workspace::{Workspace, path_key};

type Declarations = HashMap<String, Range>;

pub struct Engine {
    root: PathBuf,
    workspace: Option<Workspace>,
    parser: Parser,
    documents: HashMap<PathBuf, Source>,
    disk_index: HashMap<PathBuf, Declarations>,
}

impl Engine {
    pub fn new(root: PathBuf) -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .expect("Go grammar must match the tree-sitter runtime");
        Self {
            root,
            workspace: None,
            parser,
            documents: HashMap::new(),
            disk_index: HashMap::new(),
        }
    }

    pub fn open(&mut self, path: PathBuf, text: String) -> Result<()> {
        if path.extension().is_some_and(|extension| extension == "go") {
            let key = path_key(&path);
            let source = Source::parse(&mut self.parser, text, None)?;
            self.disk_index.remove(&key);
            self.documents.insert(key, source);
        }
        Ok(())
    }

    pub fn edit(&mut self, path: &Path, edits: &[TextDocumentContentChangeEvent]) -> Result<()> {
        if let Some(source) = self.documents.get_mut(&path_key(path))
            && !edits.is_empty()
        {
            source.edit(&mut self.parser, edits)?;
        }
        Ok(())
    }

    pub fn close(&mut self, path: &Path) {
        let key = path_key(path);
        self.documents.remove(&key);
        self.disk_index.remove(&key);
    }

    pub fn definition(
        &mut self,
        path: &Path,
        position: Position,
    ) -> Result<Option<(PathBuf, Range)>> {
        let disk_source;
        let source = if let Some(source) = self.documents.get(&path_key(path)) {
            source
        } else if path.extension().is_some_and(|extension| extension == "go") {
            disk_source = read_source(&mut self.parser, path)?;
            &disk_source
        } else {
            return Ok(None);
        };

        let Some((qualifier, symbol)) = source.qualified_reference(position) else {
            return Ok(None);
        };
        if self.workspace.is_none() {
            self.workspace = Some(Workspace::load(&self.root)?);
        }
        let workspace = self.workspace.as_ref().expect("workspace is loaded");
        let Some(from) = workspace.package_of_file(path) else {
            return Ok(None);
        };
        let Some(target) = source.imports.iter().find_map(|import| {
            let package = workspace.resolve_import(from, &import.path)?;
            let name = import.alias.as_deref().unwrap_or(&package.name);
            (name == qualifier).then_some(package)
        }) else {
            return Ok(None);
        };

        for file in &target.files {
            let key = path_key(file);
            let declarations = if let Some(source) = self.documents.get(&key) {
                source.declarations()
            } else {
                match self.disk_index.entry(key) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => {
                        let source = read_source(&mut self.parser, file)?;
                        entry.insert(source.collect_declarations())
                    }
                }
            };
            if let Some(range) = declarations.get(symbol) {
                return Ok(Some((file.clone(), *range)));
            }
        }
        Ok(None)
    }
}

fn read_source(parser: &mut Parser, path: &Path) -> Result<Source> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Source::parse(parser, text, None)
}

struct Import {
    alias: Option<String>,
    path: String,
}

struct Source {
    text: String,
    lines: LineIndex,
    tree: Tree,
    imports: Vec<Import>,
    declarations: OnceCell<Declarations>,
}

impl Source {
    fn parse(parser: &mut Parser, text: String, previous: Option<&Tree>) -> Result<Self> {
        let lines = LineIndex::new(&text);
        Self::parse_with_lines(parser, text, lines, previous)
    }

    fn parse_with_lines(
        parser: &mut Parser,
        text: String,
        lines: LineIndex,
        previous: Option<&Tree>,
    ) -> Result<Self> {
        let tree = parser.parse(&text, previous).context("parsing Go source")?;
        let imports = imports(tree.root_node(), &text);
        Ok(Self {
            text,
            lines,
            tree,
            imports,
            declarations: OnceCell::new(),
        })
    }

    fn edit(
        &mut self,
        parser: &mut Parser,
        changes: &[TextDocumentContentChangeEvent],
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

        *self = Self::parse_with_lines(parser, text, lines, Some(&tree))?;
        Ok(())
    }

    fn qualified_reference(&self, position: Position) -> Option<(&str, &str)> {
        let offset = self.lines.offset(&self.text, position)?;
        let root = self.tree.root_node();
        let mut node = root.named_descendant_for_byte_range(offset, offset)?;
        loop {
            let fields = match node.kind() {
                "selector_expression" => Some(("operand", "field")),
                "qualified_type" => Some(("package", "name")),
                _ => None,
            };
            if let Some((qualifier_field, symbol_field)) = fields
                && let Some(symbol) = node.child_by_field_name(symbol_field)
                && symbol.start_byte() <= offset
                && offset <= symbol.end_byte()
            {
                let qualifier = node.child_by_field_name(qualifier_field)?;
                if !matches!(qualifier.kind(), "identifier" | "package_identifier") {
                    return None;
                }
                let qualifier = node_text(qualifier, &self.text);
                let symbol = node_text(symbol, &self.text);
                return (is_exported(symbol) && !local_binding(node, qualifier, &self.text))
                    .then_some((qualifier, symbol));
            }
            node = node.parent()?;
        }
    }

    fn declarations(&self) -> &Declarations {
        self.declarations
            .get_or_init(|| self.collect_declarations())
    }

    fn collect_declarations(&self) -> Declarations {
        let root = self.tree.root_node();
        let mut cursor = root.walk();
        let mut declarations = HashMap::new();
        for declaration in root.named_children(&mut cursor) {
            visit_declaration_names(declaration, &mut |name| {
                let symbol = node_text(name, &self.text);
                if is_exported(symbol) {
                    declarations.entry(symbol.to_owned()).or_insert_with(|| {
                        Range::new(
                            self.lines.position(&self.text, name.start_byte()),
                            self.lines.position(&self.text, name.end_byte()),
                        )
                    });
                }
            });
        }
        declarations
    }
}

fn imports(root: Node<'_>, text: &str) -> Vec<Import> {
    fn collect(node: Node<'_>, text: &str, result: &mut Vec<Import>) {
        match node.kind() {
            "import_declaration" | "import_spec_list" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    collect(child, text, result);
                }
            }
            "import_spec" => {
                let Some(path) = node.child_by_field_name("path") else {
                    return;
                };
                let literal = node_text(path, text);
                let path = if let Some(raw) =
                    literal.strip_prefix('`').and_then(|s| s.strip_suffix('`'))
                {
                    Some(raw.replace('\r', ""))
                } else {
                    serde_json::from_str::<String>(literal).ok()
                };
                let alias = node
                    .child_by_field_name("name")
                    .map(|name| node_text(name, text).to_owned());
                if let Some(path) = path
                    && !matches!(alias.as_deref(), Some("_" | "."))
                {
                    result.push(Import { alias, path });
                }
            }
            _ => {}
        }
    }

    let mut result = Vec::new();
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        collect(node, text, &mut result);
    }
    result
}

fn local_binding<'tree>(selector: Node<'tree>, name: &str, text: &str) -> bool {
    let offset = selector.start_byte();
    let binds = |node: Node<'tree>| {
        let mut found = false;
        visit_declaration_names(node, &mut |candidate| {
            found |= node_text(candidate, text) == name;
        });
        found
    };
    let list_binds = |list: Node<'tree>| {
        let mut cursor = list.walk();
        list.named_children(&mut cursor)
            .any(|node| node.kind() == "identifier" && node_text(node, text) == name)
    };
    let after_binding = |node: Node<'tree>| node.end_byte() <= offset && binds(node);

    let mut child = selector;
    while let Some(scope) = child.parent() {
        let shadowed = match scope.kind() {
            "statement_list" | "var_spec_list" | "const_declaration" | "type_declaration" => {
                let mut previous = child.prev_named_sibling();
                let mut found = false;
                while let Some(node) = previous {
                    if binds(node) {
                        found = true;
                        break;
                    }
                    previous = node.prev_named_sibling();
                }
                found
            }
            "if_statement" | "expression_switch_statement" | "for_clause" => scope
                .child_by_field_name("initializer")
                .is_some_and(after_binding),
            "for_statement" if child.kind() == "block" => {
                let mut cursor = scope.walk();
                scope
                    .named_children(&mut cursor)
                    .any(|clause| match clause.kind() {
                        "for_clause" => clause
                            .child_by_field_name("initializer")
                            .is_some_and(after_binding),
                        "range_clause" => {
                            has_short_operator(clause)
                                && clause.child_by_field_name("left").is_some_and(list_binds)
                        }
                        _ => false,
                    })
            }
            "type_switch_statement" => {
                scope
                    .child_by_field_name("initializer")
                    .is_some_and(after_binding)
                    || (matches!(child.kind(), "type_case" | "default_case")
                        && in_case_body(child, offset)
                        && scope.child_by_field_name("alias").is_some_and(list_binds))
            }
            "communication_case" if child.kind() == "statement_list" => scope
                .child_by_field_name("communication")
                .filter(|node| node.kind() == "receive_statement" && has_short_operator(*node))
                .and_then(|node| node.child_by_field_name("left"))
                .is_some_and(list_binds),
            "function_declaration" | "method_declaration" | "func_literal"
                if child.kind() == "block" =>
            {
                ["parameters", "receiver", "result", "type_parameters"]
                    .iter()
                    .filter_map(|field| scope.child_by_field_name(*field))
                    .any(binds)
            }
            "function_declaration" | "type_spec" | "type_alias" => scope
                .child_by_field_name("type_parameters")
                .filter(|list| list.start_byte() <= offset)
                .is_some_and(binds),
            _ => false,
        };
        if shadowed {
            return true;
        }
        child = scope;
    }
    false
}

fn in_case_body(case: Node<'_>, offset: usize) -> bool {
    let mut cursor = case.walk();
    case.named_children(&mut cursor)
        .any(|node| node.kind() == "statement_list" && node.start_byte() <= offset)
}
fn has_short_operator(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|child| child.kind() == ":=")
}

fn visit_declaration_names<'tree>(node: Node<'tree>, visit: &mut impl FnMut(Node<'tree>)) {
    let mut cursor = node.walk();
    match node.kind() {
        "var_declaration"
        | "const_declaration"
        | "type_declaration"
        | "var_spec_list"
        | "parameter_list"
        | "type_parameter_list"
        | "labeled_statement" => {
            for child in node.named_children(&mut cursor) {
                visit_declaration_names(child, visit);
            }
        }
        "short_var_declaration" => {
            if let Some(left) = node.child_by_field_name("left") {
                for name in left
                    .named_children(&mut cursor)
                    .filter(|node| node.kind() == "identifier")
                {
                    visit(name);
                }
            }
        }
        "function_declaration"
        | "type_spec"
        | "type_alias"
        | "var_spec"
        | "const_spec"
        | "parameter_declaration"
        | "variadic_parameter_declaration"
        | "type_parameter_declaration" => {
            for name in node
                .children_by_field_name("name", &mut cursor)
                .filter(Node::is_named)
            {
                visit(name);
            }
        }
        _ => {}
    }
}

fn is_exported(symbol: &str) -> bool {
    symbol.chars().next().is_some_and(char::is_uppercase)
}

fn node_text<'text>(node: Node<'_>, text: &'text str) -> &'text str {
    &text[node.byte_range()]
}

#[derive(Clone)]
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(index, _)| index + 1))
            .collect();
        Self { starts }
    }

    fn offset(&self, text: &str, position: Position) -> Option<usize> {
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

    fn point(&self, offset: usize) -> Point {
        let line = self.starts.partition_point(|start| *start <= offset) - 1;
        Point::new(line, offset - self.starts[line])
    }

    fn position(&self, text: &str, offset: usize) -> Position {
        let point = self.point(offset);
        let column = text[self.starts[point.row]..offset].encode_utf16().count();
        Position::new(point.row as u32, column as u32)
    }

    fn edit(&mut self, start: usize, end: usize, inserted: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_exported_symbols_in_grouped_declarations() {
        let mut engine = Engine::new(PathBuf::new());
        let source = Source::parse(
            &mut engine.parser,
            "package test\nvar (First, Second = 1, 2)\nconst (Third, fourth = 3, 4)\ntype (Fifth int)\nfunc Sixth() {}\n".into(),
            None,
        )
        .unwrap();
        let names = source.declarations();
        for name in ["First", "Second", "Third", "Fifth", "Sixth"] {
            assert!(names.contains_key(name), "missing {name}");
        }
        assert!(!names.contains_key("fourth"));
    }

    #[test]
    fn import_qualifier_respects_shadowing_and_utf16() {
        let mut engine = Engine::new(PathBuf::new());
        let text = concat!(
            "package test\n",
            "import alias \"example.test/lib\"\n",
            "func a() { _ = \"😀\"; alias.Foo() }\n",
            "func b(alias T) { alias.Foo() }\n",
            "func c() { alias := T{}; alias.Foo() }\n",
            "func d() { if alias := T{}; true { alias.Foo() }; alias.Foo() }\n",
        );
        let source = Source::parse(&mut engine.parser, text.into(), None).unwrap();
        let queries: Vec<_> = text
            .match_indices("Foo")
            .map(|(offset, _)| source.qualified_reference(source.lines.position(text, offset)))
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
}
