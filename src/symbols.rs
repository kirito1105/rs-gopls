use std::collections::HashMap;

use lsp_types::Range;
use tree_sitter::{Node, Tree};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TypeRef {
    pub(crate) qualifier: Option<String>,
    pub(crate) name: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Symbol {
    pub(crate) range: Range,
    pub(crate) kind: SymbolKind,
}

#[derive(Clone, Debug)]
pub(crate) enum SymbolKind {
    Value(Option<TypeRef>),
    Type,
    Function(Vec<Option<TypeRef>>),
}

#[derive(Default)]
pub(crate) struct TypeDetails {
    pub(crate) alias: Option<TypeRef>,
    pub(crate) embedded: Vec<TypeRef>,
}

#[derive(Default)]
pub(crate) struct FileSymbols {
    pub(crate) package_range: Option<Range>,
    declarations: HashMap<String, Symbol>,
    members: HashMap<String, HashMap<String, Symbol>>,
    types: HashMap<String, TypeDetails>,
}

impl FileSymbols {
    pub(crate) fn declaration(&self, name: &str) -> Option<&Symbol> {
        self.declarations.get(name)
    }

    pub(crate) fn member(&self, ty: &str, name: &str) -> Option<&Symbol> {
        self.members.get(ty)?.get(name)
    }

    pub(crate) fn type_details(&self, ty: &str) -> Option<&TypeDetails> {
        self.types.get(ty)
    }
}

pub(crate) fn collect(tree: &Tree, text: &str, range: impl Fn(Node<'_>) -> Range) -> FileSymbols {
    let mut indexer = Indexer {
        text,
        range: &range,
        symbols: FileSymbols::default(),
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    for declaration in root.named_children(&mut cursor) {
        indexer.declaration(declaration);
    }
    indexer.symbols
}

struct Indexer<'a> {
    text: &'a str,
    range: &'a dyn Fn(Node<'_>) -> Range,
    symbols: FileSymbols,
}

impl Indexer<'_> {
    fn declaration(&mut self, declaration: Node<'_>) {
        let mut cursor = declaration.walk();
        match declaration.kind() {
            "package_clause" => {
                self.symbols.package_range = declaration
                    .named_children(&mut cursor)
                    .find(|node| node.kind() == "package_identifier")
                    .map(self.range);
            }
            "var_declaration" | "var_spec_list" | "const_declaration" | "type_declaration" => {
                for spec in declaration.named_children(&mut cursor) {
                    self.declaration(spec);
                }
            }
            "var_spec" | "const_spec" => self.value_declaration(declaration),
            "type_spec" | "type_alias" => self.type_declaration(declaration),
            "function_declaration" => {
                if let Some(name) = declaration.child_by_field_name("name") {
                    let results = function_results(declaration, self.text);
                    self.insert_declaration(name, SymbolKind::Function(results));
                }
            }
            "method_declaration" => self.method_declaration(declaration),
            _ => {}
        }
    }

    fn value_declaration(&mut self, declaration: Node<'_>) {
        let mut cursor = declaration.walk();
        let names: Vec<_> = declaration
            .children_by_field_name("name", &mut cursor)
            .filter(|node| node.kind() == "identifier")
            .collect();
        let ty = declaration.child_by_field_name("type");
        let explicit_type = ty.and_then(|node| named_type(node, self.text));
        let values: Vec<_> = if ty.is_none() {
            declaration
                .child_by_field_name("value")
                .map(|value| {
                    value
                        .named_children(&mut value.walk())
                        .filter(|node| !node.is_extra())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let paired_values = names.len() == values.len();
        for (index, name) in names.into_iter().enumerate() {
            let ty = if paired_values {
                composite_value_type(values[index], self.text)
            } else {
                explicit_type.clone()
            };
            self.insert_declaration(name, SymbolKind::Value(ty));
        }
    }

    fn insert_declaration(&mut self, name: Node<'_>, kind: SymbolKind) {
        let key = node_text(name, self.text);
        if key.is_empty() || key == "_" {
            return;
        }
        self.symbols
            .declarations
            .entry(key.to_owned())
            .or_insert_with(|| Symbol {
                range: (self.range)(name),
                kind,
            });
    }

    fn type_declaration(&mut self, declaration: Node<'_>) {
        let (Some(name), Some(ty)) = (
            declaration.child_by_field_name("name"),
            declaration.child_by_field_name("type"),
        ) else {
            return;
        };
        self.insert_declaration(name, SymbolKind::Type);

        let owner = node_text(name, self.text);
        let mut details = TypeDetails::default();
        if declaration.kind() == "type_alias" {
            details.alias = named_type(ty, self.text);
        }
        let ty = unparenthesized_type(ty);
        if let Some(ty) = ty {
            match ty.kind() {
                "struct_type" => self.struct_members(owner, ty, &mut details),
                "interface_type" => self.interface_members(owner, ty, &mut details),
                _ => {}
            }
        }
        self.symbols
            .types
            .entry(owner.to_owned())
            .or_insert(details);
    }

    fn struct_members(&mut self, owner: &str, ty: Node<'_>, details: &mut TypeDetails) {
        let mut cursor = ty.walk();
        let Some(list) = ty
            .named_children(&mut cursor)
            .find(|node| node.kind() == "field_declaration_list")
        else {
            return;
        };
        let mut cursor = list.walk();
        for field in list
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "field_declaration")
        {
            let field_type = field.child_by_field_name("type");
            let ty = field_type.and_then(|node| named_type(node, self.text));
            let mut name_cursor = field.walk();
            let mut names = field
                .children_by_field_name("name", &mut name_cursor)
                .filter(|node| node.kind() == "field_identifier")
                .peekable();
            if names.peek().is_some() {
                for name in names {
                    self.insert_member(owner, name, SymbolKind::Value(ty.clone()));
                }
            } else if let (Some(ty), Some((_, name))) = (ty, field_type.and_then(named_type_nodes))
            {
                self.insert_member(owner, name, SymbolKind::Value(Some(ty.clone())));
                details.embedded.push(ty);
            }
        }
    }

    fn interface_members(&mut self, owner: &str, ty: Node<'_>, details: &mut TypeDetails) {
        let mut cursor = ty.walk();
        for element in ty.named_children(&mut cursor) {
            match element.kind() {
                "method_elem" => {
                    if let Some(name) = element.child_by_field_name("name") {
                        let results = function_results(element, self.text);
                        self.insert_member(owner, name, SymbolKind::Function(results));
                    }
                }
                "type_elem" => {
                    // A union or an approximation constrains a type set, without embedding a method set.
                    let mut cursor = element.walk();
                    let mut terms = element
                        .named_children(&mut cursor)
                        .filter(|term| !term.is_extra());
                    if let Some(term) = terms.next()
                        && terms.next().is_none()
                        && let Some(ty) = named_type(term, self.text)
                    {
                        details.embedded.push(ty);
                    }
                }
                _ => {}
            }
        }
    }

    fn method_declaration(&mut self, declaration: Node<'_>) {
        let (Some(receiver), Some(name)) = (
            declaration.child_by_field_name("receiver"),
            declaration.child_by_field_name("name"),
        ) else {
            return;
        };
        let mut cursor = receiver.walk();
        let mut parameters = receiver
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "parameter_declaration");
        let Some(parameter) = parameters.next() else {
            return;
        };
        if parameters.next().is_some() {
            return;
        }
        let Some((None, owner)) = parameter
            .child_by_field_name("type")
            .and_then(named_type_nodes)
        else {
            return;
        };
        let results = function_results(declaration, self.text);
        self.insert_member(
            node_text(owner, self.text),
            name,
            SymbolKind::Function(results),
        );
    }

    fn insert_member(&mut self, owner: &str, name: Node<'_>, kind: SymbolKind) {
        let key = node_text(name, self.text);
        if key.is_empty() || key == "_" {
            return;
        }
        self.symbols
            .members
            .entry(owner.to_owned())
            .or_default()
            .entry(key.to_owned())
            .or_insert_with(|| Symbol {
                range: (self.range)(name),
                kind,
            });
    }
}

pub(crate) fn named_type(node: Node<'_>, text: &str) -> Option<TypeRef> {
    let (qualifier, name) = named_type_nodes(node)?;
    let name = node_text(name, text);
    let qualifier = qualifier.map(|qualifier| node_text(qualifier, text));
    if name.is_empty() || name == "_" || qualifier.is_some_and(|qualifier| qualifier.is_empty()) {
        return None;
    }
    Some(TypeRef {
        qualifier: qualifier.map(str::to_owned),
        name: name.to_owned(),
    })
}

fn named_type_nodes(mut node: Node<'_>) -> Option<(Option<Node<'_>>, Node<'_>)> {
    loop {
        node = match node.kind() {
            "type_identifier" | "identifier" => return Some((None, node)),
            "qualified_type" => {
                return Some((
                    Some(node.child_by_field_name("package")?),
                    node.child_by_field_name("name")?,
                ));
            }
            "pointer_type" | "parenthesized_type" => only_named_child(node)?,
            "generic_type" => node.child_by_field_name("type")?,
            _ => return None,
        };
    }
}

fn unparenthesized_type(mut node: Node<'_>) -> Option<Node<'_>> {
    while node.kind() == "parenthesized_type" {
        node = only_named_child(node)?;
    }
    Some(node)
}

fn only_named_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    let mut children = node
        .named_children(&mut cursor)
        .filter(|node| !node.is_extra());
    let child = children.next()?;
    children.next().is_none().then_some(child)
}

fn composite_value_type(mut value: Node<'_>, text: &str) -> Option<TypeRef> {
    loop {
        value = match value.kind() {
            "composite_literal" => {
                return value
                    .child_by_field_name("type")
                    .and_then(|ty| named_type(ty, text));
            }
            "parenthesized_expression" => only_named_child(value)?,
            "unary_expression"
                if value
                    .child_by_field_name("operator")
                    .is_some_and(|op| op.kind() == "&") =>
            {
                value.child_by_field_name("operand")?
            }
            _ => return None,
        };
    }
}

fn function_results(function: Node<'_>, text: &str) -> Vec<Option<TypeRef>> {
    let Some(result) = function.child_by_field_name("result") else {
        return Vec::new();
    };
    if result.kind() != "parameter_list" {
        return vec![named_type(result, text)];
    }
    let mut results = Vec::new();
    let mut cursor = result.walk();
    for parameter in result
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "parameter_declaration")
    {
        let ty = parameter
            .child_by_field_name("type")
            .and_then(|node| named_type(node, text));
        let mut name_cursor = parameter.walk();
        let count = parameter
            .children_by_field_name("name", &mut name_cursor)
            .filter(|node| node.kind() == "identifier")
            .count()
            .max(1);
        results.extend(std::iter::repeat_n(ty, count));
    }
    results
}

fn node_text<'text>(node: Node<'_>, text: &'text str) -> &'text str {
    &text[node.byte_range()]
}
