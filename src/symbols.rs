use std::collections::HashMap;
use std::sync::Arc;

use lsp_types::Range;
use tree_sitter::{Node, Tree};

#[derive(Clone, Debug)]
pub(crate) struct TypeRef {
    pub(crate) pointers: u8,
    pub(crate) kind: TypeRefKind,
}

#[derive(Clone, Debug)]
pub(crate) enum TypeRefKind {
    Named(NamedType),
    Container(Box<Container<TypeRef>>),
    Inline(Arc<InlineType>),
}

#[derive(Clone, Debug)]
pub(crate) struct NamedType {
    pub(crate) qualifier: Option<String>,
    pub(crate) name: String,
    pub(crate) arguments: Vec<Option<TypeRef>>,
}

#[derive(Clone, Debug)]
pub(crate) enum Container<T> {
    Slice(T),
    Array(T),
    Map { key: T, value: T },
    Channel { element: T, receive: bool },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct InlineType {
    pub(crate) origin: Range,
    pub(crate) interface: bool,
    pub(crate) members: HashMap<String, Symbol>,
    pub(crate) embedded: Vec<TypeRef>,
}

#[derive(Clone, Debug)]
pub(crate) struct Symbol {
    pub(crate) range: Range,
    pub(crate) kind: SymbolKind,
    pub(crate) pointer_receiver: bool,
    pub(crate) receiver_parameters: Vec<Option<String>>,
    pub(crate) addressable: bool,
}

impl Symbol {
    fn new(range: Range, kind: SymbolKind, addressable: bool) -> Self {
        Self {
            range,
            kind,
            pointer_receiver: false,
            receiver_parameters: Vec::new(),
            addressable,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum SymbolKind {
    Value(Option<TypeRef>),
    Type,
    Function(Vec<Option<TypeRef>>),
}

#[derive(Clone, Default)]
pub(crate) struct TypeDetails {
    pub(crate) alias: Option<TypeRef>,
    pub(crate) underlying: Option<TypeRef>,
    pub(crate) interface: bool,
    pub(crate) embedded: Vec<TypeRef>,
    pub(crate) parameters: Vec<String>,
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
                    let results = function_results(declaration, self.text, self.range);
                    self.insert_declaration(name, SymbolKind::Function(results), false);
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
        let explicit_type = ty.and_then(|node| type_ref(node, self.text, self.range));
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
                composite_value_type(values[index], self.text, self.range)
            } else {
                explicit_type.clone()
            };
            self.insert_declaration(
                name,
                SymbolKind::Value(ty),
                declaration.kind() == "var_spec",
            );
        }
    }

    fn insert_declaration(&mut self, name: Node<'_>, kind: SymbolKind, addressable: bool) {
        let key = node_text(name, self.text);
        if key.is_empty() || key == "_" {
            return;
        }
        self.symbols
            .declarations
            .entry(key.to_owned())
            .or_insert_with(|| Symbol::new((self.range)(name), kind, addressable));
    }

    fn type_declaration(&mut self, declaration: Node<'_>) {
        let (Some(name), Some(ty)) = (
            declaration.child_by_field_name("name"),
            declaration.child_by_field_name("type"),
        ) else {
            return;
        };
        self.insert_declaration(name, SymbolKind::Type, false);

        let owner = node_text(name, self.text);
        let mut details = TypeDetails {
            parameters: type_parameters(declaration, self.text),
            ..TypeDetails::default()
        };
        let underlying = type_ref(ty, self.text, self.range);
        if declaration.kind() == "type_alias" {
            details.alias = underlying;
        } else if let Some(TypeRef {
            pointers: 0,
            kind: TypeRefKind::Inline(inline),
        }) = underlying
        {
            details.interface = inline.interface;
            details.embedded = inline.embedded.clone();
            let members = self.symbols.members.entry(owner.to_owned()).or_default();
            for (name, symbol) in &inline.members {
                members
                    .entry(name.clone())
                    .or_insert_with(|| symbol.clone());
            }
        } else {
            details.underlying = underlying;
        }
        self.symbols
            .types
            .entry(owner.to_owned())
            .or_insert(details);
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
        let Some(owner) = parameter
            .child_by_field_name("type")
            .and_then(|ty| type_ref(ty, self.text, self.range))
        else {
            return;
        };
        let TypeRefKind::Named(named) = owner.kind else {
            return;
        };
        if named.qualifier.is_some() || owner.pointers > 1 {
            return;
        }
        let results = function_results(declaration, self.text, self.range);
        let mut symbol = Symbol::new((self.range)(name), SymbolKind::Function(results), false);
        symbol.pointer_receiver = owner.pointers == 1;
        symbol.receiver_parameters = named
            .arguments
            .into_iter()
            .map(|argument| {
                let TypeRef {
                    pointers: 0,
                    kind: TypeRefKind::Named(parameter),
                } = argument?
                else {
                    return None;
                };
                (parameter.qualifier.is_none() && parameter.arguments.is_empty())
                    .then_some(parameter.name)
            })
            .collect();
        self.insert_member(&named.name, name, symbol);
    }

    fn insert_member(&mut self, owner: &str, name: Node<'_>, symbol: Symbol) {
        let key = node_text(name, self.text);
        if key.is_empty() || key == "_" {
            return;
        }
        self.symbols
            .members
            .entry(owner.to_owned())
            .or_default()
            .entry(key.to_owned())
            .or_insert(symbol);
    }
}

pub(crate) fn type_ref(
    node: Node<'_>,
    text: &str,
    range: &dyn Fn(Node<'_>) -> Range,
) -> Option<TypeRef> {
    let node = unparenthesized_type(node)?;
    let parse = |field| type_ref(node.child_by_field_name(field)?, text, range);
    let kind = match node.kind() {
        "pointer_type" => {
            let mut inner = type_ref(only_named_child(node)?, text, range)?;
            inner.pointers = inner.pointers.checked_add(1)?;
            return Some(inner);
        }
        "slice_type" => TypeRefKind::Container(Box::new(Container::Slice(parse("element")?))),
        "array_type" | "implicit_length_array_type" => {
            TypeRefKind::Container(Box::new(Container::Array(parse("element")?)))
        }
        "map_type" => TypeRefKind::Container(Box::new(Container::Map {
            key: parse("key")?,
            value: parse("value")?,
        })),
        "channel_type" => TypeRefKind::Container(Box::new(Container::Channel {
            element: parse("value")?,
            receive: node
                .children(&mut node.walk())
                .filter(|child| !child.is_extra())
                .nth(1)
                .is_some_and(|second| second.kind() != "<-"),
        })),
        "struct_type" | "interface_type" => {
            TypeRefKind::Inline(Arc::new(inline_type(node, text, range)))
        }
        _ => TypeRefKind::Named(named_type(node, text, range)?),
    };
    Some(TypeRef { pointers: 0, kind })
}

fn named_type(node: Node<'_>, text: &str, range: &dyn Fn(Node<'_>) -> Range) -> Option<NamedType> {
    let syntax = named_type_syntax(node)?;
    let name = node_text(syntax.name, text);
    let qualifier = syntax.qualifier.map(|qualifier| node_text(qualifier, text));
    if name.is_empty() || name == "_" || qualifier.is_some_and(|qualifier| qualifier.is_empty()) {
        return None;
    }
    let arguments = syntax
        .arguments
        .map(|list| {
            list.named_children(&mut list.walk())
                .filter(|node| node.kind() == "type_elem")
                .map(|node| only_named_child(node).and_then(|ty| type_ref(ty, text, range)))
                .collect()
        })
        .unwrap_or_default();
    Some(NamedType {
        qualifier: qualifier.map(str::to_owned),
        name: name.to_owned(),
        arguments,
    })
}

fn inline_type(node: Node<'_>, text: &str, range: &dyn Fn(Node<'_>) -> Range) -> InlineType {
    let mut inline = InlineType {
        origin: range(node),
        interface: node.kind() == "interface_type",
        ..InlineType::default()
    };
    let mut cursor = node.walk();
    if inline.interface {
        for element in node.named_children(&mut cursor) {
            match element.kind() {
                "method_elem" => {
                    if let Some(name) = element.child_by_field_name("name") {
                        let results = function_results(element, text, range);
                        let symbol = Symbol::new(range(name), SymbolKind::Function(results), false);
                        inline
                            .members
                            .entry(node_text(name, text).to_owned())
                            .or_insert(symbol);
                    }
                }
                "type_elem" => {
                    // A union or approximation constrains the type set without embedding methods.
                    if let Some(term) = only_named_child(element)
                        && let Some(reference) = type_ref(term, text, range)
                        && matches!(reference.kind, TypeRefKind::Named(_))
                    {
                        inline.embedded.push(reference);
                    }
                }
                _ => {}
            }
        }
        return inline;
    }
    let Some(list) = node
        .named_children(&mut cursor)
        .find(|node| node.kind() == "field_declaration_list")
    else {
        return inline;
    };
    for field in list
        .named_children(&mut list.walk())
        .filter(|node| node.kind() == "field_declaration")
    {
        let field_type = field.child_by_field_name("type");
        let ty = field_type.and_then(|node| type_ref(node, text, range));
        let mut name_cursor = field.walk();
        let mut names = field
            .children_by_field_name("name", &mut name_cursor)
            .filter(|node| node.kind() == "field_identifier")
            .peekable();
        if names.peek().is_some() {
            for name in names {
                let key = node_text(name, text);
                if !key.is_empty() && key != "_" {
                    let symbol = Symbol::new(range(name), SymbolKind::Value(ty.clone()), true);
                    inline.members.entry(key.to_owned()).or_insert(symbol);
                }
            }
        } else if let (Some(mut ty), Some(syntax)) = (ty, field_type.and_then(named_type_syntax)) {
            // The optional star belongs to the embedded field, not its named type child.
            if field
                .children(&mut field.walk())
                .any(|child| child.kind() == "*")
            {
                ty.pointers += 1;
            }
            let name = syntax.name;
            let symbol = Symbol::new(range(name), SymbolKind::Value(Some(ty.clone())), true);
            inline
                .members
                .entry(node_text(name, text).to_owned())
                .or_insert(symbol);
            inline.embedded.push(ty);
        }
    }
    inline
}

struct NamedTypeSyntax<'tree> {
    qualifier: Option<Node<'tree>>,
    name: Node<'tree>,
    arguments: Option<Node<'tree>>,
}

fn named_type_syntax(mut node: Node<'_>) -> Option<NamedTypeSyntax<'_>> {
    let mut arguments = None;
    loop {
        node = match node.kind() {
            "type_identifier" | "identifier" => {
                return Some(NamedTypeSyntax {
                    qualifier: None,
                    name: node,
                    arguments,
                });
            }
            "qualified_type" => {
                return Some(NamedTypeSyntax {
                    qualifier: Some(node.child_by_field_name("package")?),
                    name: node.child_by_field_name("name")?,
                    arguments,
                });
            }
            "parenthesized_type" => only_named_child(node)?,
            "generic_type" => {
                arguments = Some(node.child_by_field_name("type_arguments")?);
                node.child_by_field_name("type")?
            }
            _ => return None,
        };
    }
}

fn type_parameters(declaration: Node<'_>, text: &str) -> Vec<String> {
    let mut parameters = Vec::new();
    if let Some(list) = declaration.child_by_field_name("type_parameters") {
        let mut cursor = list.walk();
        for parameter in list.named_children(&mut cursor) {
            let mut cursor = parameter.walk();
            let names = parameter
                .children_by_field_name("name", &mut cursor)
                .filter(|node| node.kind() == "identifier");
            parameters.extend(names.map(|name| node_text(name, text).to_owned()));
        }
    }
    parameters
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

fn composite_value_type(
    mut value: Node<'_>,
    text: &str,
    range: &dyn Fn(Node<'_>) -> Range,
) -> Option<TypeRef> {
    let mut pointers = 0_u8;
    loop {
        value = match value.kind() {
            "composite_literal" => {
                let mut ty = type_ref(value.child_by_field_name("type")?, text, range)?;
                ty.pointers = ty.pointers.checked_add(pointers)?;
                return Some(ty);
            }
            "parenthesized_expression" => only_named_child(value)?,
            "unary_expression"
                if value
                    .child_by_field_name("operator")
                    .is_some_and(|op| op.kind() == "&") =>
            {
                pointers = pointers.checked_add(1)?;
                value.child_by_field_name("operand")?
            }
            _ => return None,
        };
    }
}

fn function_results(
    function: Node<'_>,
    text: &str,
    range: &dyn Fn(Node<'_>) -> Range,
) -> Vec<Option<TypeRef>> {
    let Some(result) = function.child_by_field_name("result") else {
        return Vec::new();
    };
    if result.kind() != "parameter_list" {
        return vec![type_ref(result, text, range)];
    }
    let mut results = Vec::new();
    let mut cursor = result.walk();
    for parameter in result
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "parameter_declaration")
    {
        let ty = parameter
            .child_by_field_name("type")
            .and_then(|node| type_ref(node, text, range));
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
