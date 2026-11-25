use std::cell::OnceCell;
use std::collections::{HashMap, hash_map::Entry};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use lsp_types::{Position, Range, TextDocumentContentChangeEvent};
use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

use crate::scope::lexical_binding;
use crate::symbols::{self, FileSymbols, Symbol, SymbolKind, TypeRef};
use crate::workspace::{Package, Workspace, path_key};

struct FileIndex {
    symbols: FileSymbols,
    imports: Vec<Import>,
}

#[derive(Clone, Copy)]
enum ReferenceQuery<'tree> {
    Name(Node<'tree>),
    Member {
        receiver: Node<'tree>,
        name: Node<'tree>,
    },
    Import(Node<'tree>),
    LiteralField {
        ty: Node<'tree>,
        name: Node<'tree>,
    },
}

pub struct Engine {
    root: PathBuf,
    workspace: Option<Workspace>,
    parser: Parser,
    documents: HashMap<PathBuf, Source>,
    disk_index: HashMap<PathBuf, FileIndex>,
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

        let Some(reference) = source.reference_at(position) else {
            return Ok(None);
        };
        if let ReferenceQuery::Name(name) = reference {
            let declaration = if is_declaration_name(name) {
                Some(name)
            } else {
                lexical_binding(name, node_text(name, &source.text), &source.text)
                    .map(|binding| binding.name)
            };
            if let Some(declaration) = declaration {
                return Ok(Some((path.to_owned(), source.range(declaration))));
            }
        }
        if self.workspace.is_none() {
            self.workspace = Some(Workspace::load(&self.root)?);
        }
        let workspace = self.workspace.as_ref().expect("workspace is loaded");
        let Some(from) = workspace.package_of_file(path) else {
            return Ok(None);
        };
        let mut indexes = IndexReader {
            parser: &mut self.parser,
            documents: &self.documents,
            disk_index: &mut self.disk_index,
        };

        match reference {
            ReferenceQuery::Name(name) => {
                let identifier = node_text(name, &source.text);
                if let Some((import, package)) =
                    source.imported_package(workspace, from, identifier)
                {
                    if let Some(binding) = import.binding {
                        return Ok(Some((path.to_owned(), binding)));
                    }
                    return indexes.find(package, |index| index.symbols.package_range);
                }
                indexes.find(from, |index| {
                    index
                        .symbols
                        .declaration(identifier)
                        .map(|symbol| symbol.range)
                })
            }
            ReferenceQuery::Member { receiver, name } => {
                let mut resolver = Resolver { workspace, indexes };
                let member =
                    resolver.selector(from, source, receiver, node_text(name, &source.text), 0)?;
                Ok(member.map(ResolvedSymbol::location))
            }
            ReferenceQuery::LiteralField { ty, name } => {
                let mut resolver = Resolver { workspace, indexes };
                let Some(ty) = resolver.source_type(from, source, ty) else {
                    return Ok(None);
                };
                let member = resolver.member(from, &ty, node_text(name, &source.text))?;
                Ok(member.map(ResolvedSymbol::location))
            }
            ReferenceQuery::Import(literal) => {
                let Some(import_path) = import_path(node_text(literal, &source.text)) else {
                    return Ok(None);
                };
                let Some(target) = workspace.resolve_import(from, &import_path) else {
                    return Ok(None);
                };
                indexes.find(target, |index| index.symbols.package_range)
            }
        }
    }
}

struct IndexReader<'a> {
    parser: &'a mut Parser,
    documents: &'a HashMap<PathBuf, Source>,
    disk_index: &'a mut HashMap<PathBuf, FileIndex>,
}

impl IndexReader<'_> {
    fn file_index(&mut self, file: &Path) -> Result<&FileIndex> {
        let key = path_key(file);
        if let Some(source) = self.documents.get(&key) {
            return Ok(source.declarations());
        }
        Ok(match self.disk_index.entry(key) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let source = read_source(self.parser, file)?;
                entry.insert(source.collect_declarations())
            }
        })
    }

    fn find(
        &mut self,
        package: &Package,
        lookup: impl Fn(&FileIndex) -> Option<Range>,
    ) -> Result<Option<(PathBuf, Range)>> {
        for file in &package.files {
            if let Some(range) = lookup(self.file_index(file)?) {
                return Ok(Some((file.clone(), range)));
            }
        }
        Ok(None)
    }
}

const MAX_TYPE_DEPTH: usize = 32;

struct TypeIdentity<'w> {
    package: &'w Package,
    name: String,
}

struct ResolvedSymbol<'w> {
    package: &'w Package,
    file: PathBuf,
    symbol: Symbol,
}

impl ResolvedSymbol<'_> {
    fn location(self) -> (PathBuf, Range) {
        (self.file, self.symbol.range)
    }
}

struct Resolver<'w, 'idx> {
    workspace: &'w Workspace,
    indexes: IndexReader<'idx>,
}

impl<'w> Resolver<'w, '_> {
    fn selector(
        &mut self,
        from: &'w Package,
        source: &Source,
        receiver: Node<'_>,
        name: &str,
        depth: usize,
    ) -> Result<Option<ResolvedSymbol<'w>>> {
        if matches!(receiver.kind(), "identifier" | "package_identifier") {
            let qualifier = node_text(receiver, &source.text);
            if lexical_binding(receiver, qualifier, &source.text).is_none()
                && let Some((_, package)) = source.imported_package(self.workspace, from, qualifier)
            {
                return if is_exported(name) {
                    self.find_symbol(package, name, None)
                } else {
                    Ok(None)
                };
            }
        }

        let Some(ty) = self.infer_type(from, source, receiver, depth + 1)? else {
            return Ok(None);
        };
        self.member(from, &ty, name)
    }

    fn member(
        &mut self,
        from: &Package,
        ty: &TypeIdentity<'w>,
        name: &str,
    ) -> Result<Option<ResolvedSymbol<'w>>> {
        if !is_exported(name) && !std::ptr::eq(from, ty.package) {
            return Ok(None);
        }
        self.find_symbol(ty.package, name, Some(&ty.name))
    }

    fn find_symbol(
        &mut self,
        package: &'w Package,
        name: &str,
        receiver: Option<&str>,
    ) -> Result<Option<ResolvedSymbol<'w>>> {
        for file in &package.files {
            let index = &self.indexes.file_index(file)?.symbols;
            let symbol = match receiver {
                Some(ty) => index.member(ty, name),
                None => index.declaration(name),
            };
            if let Some(symbol) = symbol {
                return Ok(Some(ResolvedSymbol {
                    package,
                    file: file.clone(),
                    symbol: symbol.clone(),
                }));
            }
        }
        Ok(None)
    }

    fn infer_type(
        &mut self,
        from: &'w Package,
        source: &Source,
        expression: Node<'_>,
        depth: usize,
    ) -> Result<Option<TypeIdentity<'w>>> {
        if depth >= MAX_TYPE_DEPTH {
            return Ok(None);
        }
        match expression.kind() {
            "identifier" => {
                let name = node_text(expression, &source.text);
                if let Some(binding) = lexical_binding(expression, name, &source.text) {
                    if let Some(ty) = binding.explicit_type {
                        return Ok(self.source_type(from, source, ty));
                    }
                    return match binding.initializer {
                        Some(initializer) if binding.result_index == 0 => {
                            self.infer_type(from, source, initializer, depth + 1)
                        }
                        _ => Ok(None),
                    };
                }
                let Some(symbol) = self.find_symbol(from, name, None)? else {
                    return Ok(None);
                };
                self.value_type(symbol)
            }
            "composite_literal" => Ok(expression
                .child_by_field_name("type")
                .and_then(|ty| self.source_type(from, source, ty))),
            "unary_expression" | "parenthesized_expression" => {
                let inner = expression
                    .child_by_field_name("operand")
                    .or_else(|| expression.named_child(0));
                match inner {
                    Some(inner)
                        if expression.kind() == "parenthesized_expression"
                            || matches!(
                                node_text(expression, &source.text).as_bytes().first(),
                                Some(b'&' | b'*')
                            ) =>
                    {
                        self.infer_type(from, source, inner, depth + 1)
                    }
                    _ => Ok(None),
                }
            }
            "selector_expression" => {
                let Some(receiver) = expression.child_by_field_name("operand") else {
                    return Ok(None);
                };
                let Some(name) = expression.child_by_field_name("field") else {
                    return Ok(None);
                };
                let Some(symbol) = self.selector(
                    from,
                    source,
                    receiver,
                    node_text(name, &source.text),
                    depth + 1,
                )?
                else {
                    return Ok(None);
                };
                self.value_type(symbol)
            }
            _ => Ok(None),
        }
    }

    fn source_type(
        &self,
        from: &'w Package,
        source: &Source,
        ty: Node<'_>,
    ) -> Option<TypeIdentity<'w>> {
        let reference = symbols::named_type(ty, &source.text)?;
        if reference.qualifier.is_none()
            && lexical_binding(ty, &reference.name, &source.text).is_some()
        {
            return None;
        }
        type_identity(self.workspace, from, &source.imports, reference)
    }

    fn value_type(&mut self, resolved: ResolvedSymbol<'w>) -> Result<Option<TypeIdentity<'w>>> {
        let SymbolKind::Value(Some(reference)) = resolved.symbol.kind else {
            return Ok(None);
        };
        let index = self.indexes.file_index(&resolved.file)?;
        Ok(type_identity(
            self.workspace,
            resolved.package,
            &index.imports,
            reference,
        ))
    }
}

fn type_identity<'w>(
    workspace: &'w Workspace,
    from: &'w Package,
    imports: &[Import],
    reference: TypeRef,
) -> Option<TypeIdentity<'w>> {
    let package = match reference.qualifier {
        Some(qualifier) => imported_package(imports, workspace, from, &qualifier)?.1,
        None => from,
    };
    Some(TypeIdentity {
        package,
        name: reference.name,
    })
}

fn read_source(parser: &mut Parser, path: &Path) -> Result<Source> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Source::parse(parser, text, None)
}

#[derive(Clone)]
struct Import {
    alias: Option<String>,
    path: String,
    binding: Option<Range>,
}

struct Source {
    text: String,
    lines: LineIndex,
    tree: Tree,
    imports: Vec<Import>,
    declarations: OnceCell<FileIndex>,
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
        let imports = imports(tree.root_node(), &text, &lines);
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

    fn reference_at(&self, position: Position) -> Option<ReferenceQuery<'_>> {
        let offset = self.lines.offset(&self.text, position)?;
        let root = self.tree.root_node();
        let leaf = root.named_descendant_for_byte_range(offset, offset)?;
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
                    return Some(ReferenceQuery::Import(path));
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
            let mut ancestor = parent.parent().and_then(|node| node.parent());
            while let Some(node) = ancestor {
                if node.kind() == "composite_literal" {
                    return node
                        .child_by_field_name("type")
                        .map(|ty| ReferenceQuery::LiteralField { ty, name: leaf });
                }
                ancestor = node.parent();
            }
            return None;
        }
        (named || is_declaration_name(leaf)).then_some(ReferenceQuery::Name(leaf))
    }

    fn imported_package<'a>(
        &self,
        workspace: &'a Workspace,
        from: &Package,
        qualifier: &str,
    ) -> Option<(&Import, &'a Package)> {
        imported_package(&self.imports, workspace, from, qualifier)
    }

    fn range(&self, node: Node<'_>) -> Range {
        Range::new(
            self.lines.position(&self.text, node.start_byte()),
            self.lines.position(&self.text, node.end_byte()),
        )
    }

    fn declarations(&self) -> &FileIndex {
        self.declarations
            .get_or_init(|| self.collect_declarations())
    }

    fn collect_declarations(&self) -> FileIndex {
        FileIndex {
            symbols: symbols::collect(&self.tree, &self.text, |name| self.range(name)),
            imports: self.imports.clone(),
        }
    }
}

fn imported_package<'a, 'w>(
    imports: &'a [Import],
    workspace: &'w Workspace,
    from: &Package,
    qualifier: &str,
) -> Option<(&'a Import, &'w Package)> {
    imports.iter().find_map(|import| {
        let package = workspace.resolve_import(from, &import.path)?;
        let name = import.alias.as_deref().unwrap_or(&package.name);
        (name == qualifier).then_some((import, package))
    })
}

fn import_path(literal: &str) -> Option<String> {
    if let Some(raw) = literal.strip_prefix('`').and_then(|s| s.strip_suffix('`')) {
        Some(raw.replace('\r', ""))
    } else {
        serde_json::from_str(literal).ok()
    }
}

fn imports(root: Node<'_>, text: &str, lines: &LineIndex) -> Vec<Import> {
    fn collect(node: Node<'_>, text: &str, lines: &LineIndex, result: &mut Vec<Import>) {
        match node.kind() {
            "import_declaration" | "import_spec_list" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    collect(child, text, lines, result);
                }
            }
            "import_spec" => {
                let Some(path) = node.child_by_field_name("path") else {
                    return;
                };
                let alias = node.child_by_field_name("name");
                let binding = alias.map(|node| {
                    Range::new(
                        lines.position(text, node.start_byte()),
                        lines.position(text, node.end_byte()),
                    )
                });
                let alias = alias.map(|node| node_text(node, text).to_owned());
                if let Some(path) = import_path(node_text(path, text))
                    && !matches!(alias.as_deref(), Some("_" | "."))
                {
                    result.push(Import {
                        alias,
                        path,
                        binding,
                    });
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

fn is_declaration_name(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
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
    fn indexes_public_and_private_symbols_in_grouped_declarations() {
        let mut engine = Engine::new(PathBuf::new());
        let source = Source::parse(
            &mut engine.parser,
            "package test\nvar (First, Second = 1, 2)\nconst (Third, fourth = 3, 4)\ntype (Fifth int)\nfunc Sixth() {}\n".into(),
            None,
        )
        .unwrap();
        let names = &source.declarations().symbols;
        for name in ["First", "Second", "Third", "Fifth", "Sixth"] {
            assert!(names.declaration(name).is_some(), "missing {name}");
        }
        assert!(names.declaration("fourth").is_some());
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
}
