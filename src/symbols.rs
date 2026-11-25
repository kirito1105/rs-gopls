use std::collections::HashMap;
use std::ops;
use std::sync::Arc;

use lsp_types::Range;
use tree_sitter::{Node, Tree};

use crate::scope::{initializer_at, lexical_binding};

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
    Function(Arc<FunctionSignature>),
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
    Array { element: T, length: Option<String> },
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
    InferredValue {
        initializer: ops::Range<usize>,
        result_index: usize,
    },
    Type,
    Function(Arc<FunctionSignature>),
}

#[derive(Clone, Debug)]
pub(crate) struct FunctionSignature {
    pub(crate) results: Arc<[Option<TypeRef>]>,
    pub(crate) parameters: Vec<Option<TypeRef>>,
    pub(crate) type_parameters: Vec<GenericParameter>,
}

#[derive(Clone, Debug)]
pub(crate) struct GenericParameter {
    pub(crate) name: String,
    pub(crate) constraint: TypeConstraint,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TypeConstraint {
    pub(crate) methods: Option<TypeRef>,
    pub(crate) approximation: Option<TypeRef>,
}

#[derive(Clone, Default)]
pub(crate) struct TypeDetails {
    pub(crate) alias: Option<TypeRef>,
    pub(crate) underlying: Option<TypeRef>,
    pub(crate) interface: bool,
    pub(crate) method_names: Vec<String>,
    pub(crate) embedded: Vec<TypeRef>,
    pub(crate) parameters: Vec<GenericParameter>,
}

#[derive(Default)]
pub(crate) struct FileSymbols {
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
            "var_declaration" | "var_spec_list" | "const_declaration" | "type_declaration" => {
                for spec in declaration.named_children(&mut cursor) {
                    self.declaration(spec);
                }
            }
            "var_spec" | "const_spec" => self.value_declaration(declaration),
            "type_spec" | "type_alias" => self.type_declaration(declaration),
            "function_declaration" => {
                if let Some(name) = declaration.child_by_field_name("name") {
                    let results = function_signature(declaration, self.text, self.range);
                    self.insert_declaration(name, SymbolKind::Function(results), false);
                }
            }
            "method_declaration" => self.method_declaration(declaration),
            _ => {}
        }
    }

    fn value_declaration(&mut self, declaration: Node<'_>) {
        let mut cursor = declaration.walk();
        let names = declaration
            .children_by_field_name("name", &mut cursor)
            .filter(|node| node.kind() == "identifier");
        let ty = declaration.child_by_field_name("type");
        let explicit_type = ty.and_then(|node| type_ref(node, self.text, self.range));
        let values = declaration.child_by_field_name("value");
        let variable = declaration.kind() == "var_spec";
        for (index, name) in names.enumerate() {
            let kind = if variable
                && ty.is_none()
                && let (Some(initializer), result_index) = initializer_at(values, index)
            {
                match composite_value_type(initializer, self.text, self.range) {
                    Some(ty) if result_index == 0 => SymbolKind::Value(Some(ty)),
                    _ => SymbolKind::InferredValue {
                        initializer: initializer.byte_range(),
                        result_index,
                    },
                }
            } else {
                SymbolKind::Value(explicit_type.clone())
            };
            self.insert_declaration(name, kind, variable);
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
            parameters: type_parameters(declaration, self.text, self.range),
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
            if inline.interface {
                details.method_names = inline.members.keys().cloned().collect();
            }
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
        let results = function_signature(declaration, self.text, self.range);
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
        "pointer_type" | "unary_expression"
            if node.kind() == "pointer_type"
                || node
                    .child_by_field_name("operator")
                    .is_some_and(|op| op.kind() == "*") =>
        {
            let mut inner = type_ref(only_named_child(node)?, text, range)?;
            inner.pointers = inner.pointers.checked_add(1)?;
            return Some(inner);
        }
        "slice_type" => TypeRefKind::Container(Box::new(Container::Slice(parse("element")?))),
        "array_type" | "implicit_length_array_type" => {
            TypeRefKind::Container(Box::new(Container::Array {
                element: parse("element")?,
                length: node
                    .child_by_field_name("length")
                    .map(|length| node_text(length, text).to_owned()),
            }))
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
        "function_type" | "func_literal" => {
            TypeRefKind::Function(function_signature(node, text, range))
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
                        let results = function_signature(element, text, range);
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
            "selector_expression" => {
                let qualifier = node.child_by_field_name("operand")?;
                if qualifier.kind() != "identifier" {
                    return None;
                }
                return Some(NamedTypeSyntax {
                    qualifier: Some(qualifier),
                    name: node.child_by_field_name("field")?,
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

fn type_parameters(
    declaration: Node<'_>,
    text: &str,
    range: &dyn Fn(Node<'_>) -> Range,
) -> Vec<GenericParameter> {
    let mut parameters = Vec::new();
    if let Some(list) = declaration.child_by_field_name("type_parameters") {
        let mut cursor = list.walk();
        for parameter in list.named_children(&mut cursor) {
            let constraint = parameter
                .child_by_field_name("type")
                .map(|node| type_constraint(node, text, range))
                .unwrap_or_default();
            let mut cursor = parameter.walk();
            let names = parameter
                .children_by_field_name("name", &mut cursor)
                .filter(|node| node.kind() == "identifier");
            parameters.extend(names.map(|name| GenericParameter {
                name: node_text(name, text).to_owned(),
                constraint: constraint.clone(),
            }));
        }
    }
    parameters
}

pub(crate) fn type_constraint(
    constraint: Node<'_>,
    text: &str,
    range: &dyn Fn(Node<'_>) -> Range,
) -> TypeConstraint {
    let Some(ty) = only_named_child(constraint) else {
        return TypeConstraint::default();
    };
    let methods = match ty.kind() {
        "interface_type" | "qualified_type" | "generic_type" => type_ref(ty, text, range),
        "type_identifier" if !matches!(node_text(ty, text), "any" | "comparable") => {
            type_ref(ty, text, range)
        }
        _ => None,
    };
    let term = if ty.kind() == "interface_type" {
        let mut cursor = ty.walk();
        let mut terms = ty
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "type_elem");
        let first = terms.next();
        first
            .filter(|_| terms.next().is_none())
            .and_then(only_named_child)
    } else {
        Some(ty)
    };
    let approximation = term
        .filter(|term| term.kind() == "negated_type")
        .and_then(only_named_child)
        .and_then(|term| type_ref(term, text, range));
    TypeConstraint {
        methods,
        approximation,
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

fn function_signature(
    function: Node<'_>,
    text: &str,
    range: &dyn Fn(Node<'_>) -> Range,
) -> Arc<FunctionSignature> {
    let type_parameters = type_parameters(function, text, range);
    // Ordinary call sites only need result types.
    let parameters = if type_parameters.is_empty() {
        Vec::new()
    } else {
        function
            .child_by_field_name("parameters")
            .map(|list| parameter_types(list, |node| type_ref(node, text, range)))
            .unwrap_or_default()
    };
    Arc::new(FunctionSignature {
        results: function_results(function, text, range),
        parameters,
        type_parameters,
    })
}

fn function_results(
    function: Node<'_>,
    text: &str,
    range: &dyn Fn(Node<'_>) -> Range,
) -> Arc<[Option<TypeRef>]> {
    let Some(result) = function.child_by_field_name("result") else {
        return Arc::from([]);
    };
    let mut enclosing = function.parent();
    let mut local = false;
    while let Some(owner) = enclosing {
        if owner.kind() == "block" {
            local = true;
            break;
        }
        enclosing = owner.parent();
    }
    let parse = |node| {
        if local && has_shadowed_result_type(node, text) {
            return None;
        }
        type_ref(node, text, range)
    };
    if result.kind() != "parameter_list" {
        return Arc::from([parse(result)]);
    }
    parameter_types(result, parse).into()
}

fn parameter_types<'tree>(
    list: Node<'tree>,
    parse: impl Fn(Node<'tree>) -> Option<TypeRef>,
) -> Vec<Option<TypeRef>> {
    let mut result = Vec::new();
    let mut cursor = list.walk();
    for parameter in list.named_children(&mut cursor).filter(|node| {
        matches!(
            node.kind(),
            "parameter_declaration" | "variadic_parameter_declaration"
        )
    }) {
        let ty = parameter.child_by_field_name("type").and_then(&parse);
        let mut name_cursor = parameter.walk();
        let count = parameter
            .children_by_field_name("name", &mut name_cursor)
            .filter(|node| node.kind() == "identifier")
            .count()
            .max(1);
        result.extend(std::iter::repeat_n(ty, count));
    }
    result
}

// Package-indexed result types cannot stand in for similarly named local declarations.
fn has_shadowed_result_type(ty: Node<'_>, text: &str) -> bool {
    match ty.kind() {
        "qualified_type" => false,
        "type_identifier" => {
            lexical_binding(ty, node_text(ty, text), text).is_some_and(|binding| {
                binding
                    .name
                    .parent()
                    .is_some_and(|owner| owner.kind() != "type_parameter_declaration")
            })
        }
        _ => ty
            .named_children(&mut ty.walk())
            .any(|child| has_shadowed_result_type(child, text)),
    }
}

fn node_text<'text>(node: Node<'_>, text: &'text str) -> &'text str {
    &text[node.byte_range()]
}
