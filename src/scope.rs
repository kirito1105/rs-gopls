use tree_sitter::Node;

#[derive(Clone, Copy)]
pub(crate) struct Binding<'tree> {
    pub(crate) name: Node<'tree>,
    pub(crate) explicit_type: Option<Node<'tree>>,
    pub(crate) initializer: Option<Node<'tree>>,
    pub(crate) result_index: usize,
}

pub(crate) fn lexical_binding<'tree>(
    reference: Node<'tree>,
    name: &str,
    text: &str,
) -> Option<Binding<'tree>> {
    if name == "_" {
        return None;
    }

    let offset = reference.start_byte();
    let initializer = |scope: Node<'tree>| {
        scope
            .child_by_field_name("initializer")
            .filter(|node| node.end_byte() <= offset)
            .and_then(|node| declaration(node, name, text))
    };

    let mut child = reference;
    while let Some(scope) = child.parent() {
        let binding = match scope.kind() {
            "statement_list" | "var_spec_list" | "const_declaration" | "type_declaration" => {
                preceding_binding(child, name, text)
            }
            "if_statement" | "expression_switch_statement" | "for_clause" => initializer(scope),
            "for_statement" if child.kind() == "block" => {
                let mut cursor = scope.walk();
                scope
                    .named_children(&mut cursor)
                    .find_map(|clause| match clause.kind() {
                        "for_clause" => initializer(clause),
                        "range_clause" if has_short_operator(clause) => {
                            left_binding(clause, name, text)
                        }
                        _ => None,
                    })
            }
            "type_switch_statement" => {
                type_case_binding(scope, child, offset, name, text).or_else(|| initializer(scope))
            }
            "communication_case" if child.kind() == "statement_list" => scope
                .child_by_field_name("communication")
                .filter(|node| node.kind() == "receive_statement" && has_short_operator(*node))
                .and_then(|node| left_binding(node, name, text)),
            "function_declaration" | "method_declaration" | "func_literal"
                if child.kind() == "block" =>
            {
                function_binding(scope, name, text)
            }
            "function_declaration" => type_parameter_binding(scope, offset, name, text),
            "type_spec" | "type_alias" => type_parameter_binding(scope, offset, name, text)
                .or_else(|| {
                    let statement = scope.parent()?.parent()?;
                    (statement.kind() != "source_file")
                        .then(|| declaration(scope, name, text))
                        .flatten()
                }),
            _ => None,
        };
        if binding.is_some() {
            return binding;
        }
        child = scope;
    }
    None
}

fn preceding_binding<'tree>(child: Node<'tree>, name: &str, text: &str) -> Option<Binding<'tree>> {
    let mut previous = child.prev_named_sibling();
    let mut binding = None;
    while let Some(node) = previous {
        if let Some(found) = declaration(node, name, text) {
            if found
                .name
                .parent()
                .and_then(|list| list.parent())
                .is_none_or(|node| node.kind() != "short_var_declaration")
            {
                return Some(found);
            }
            // A short declaration can reuse an existing variable from the same block.
            binding = Some(found);
        }
        previous = node.prev_named_sibling();
    }

    let statement_list = child.parent()?;
    let block = statement_list.parent()?;
    let function = block.parent()?;
    if binding.is_some()
        && statement_list.kind() == "statement_list"
        && block.kind() == "block"
        && matches!(
            function.kind(),
            "function_declaration" | "method_declaration" | "func_literal"
        )
    {
        return function_binding(function, name, text).or(binding);
    }
    binding
}

fn function_binding<'tree>(
    function: Node<'tree>,
    name: &str,
    text: &str,
) -> Option<Binding<'tree>> {
    ["parameters", "receiver", "result", "type_parameters"]
        .into_iter()
        .filter_map(|field| function.child_by_field_name(field))
        .find_map(|node| declaration(node, name, text))
}

fn type_parameter_binding<'tree>(
    declaration_node: Node<'tree>,
    offset: usize,
    name: &str,
    text: &str,
) -> Option<Binding<'tree>> {
    declaration_node
        .child_by_field_name("type_parameters")
        .filter(|list| list.start_byte() <= offset)
        .and_then(|list| declaration(list, name, text))
}

fn type_case_binding<'tree>(
    switch: Node<'tree>,
    case: Node<'tree>,
    offset: usize,
    name: &str,
    text: &str,
) -> Option<Binding<'tree>> {
    if !matches!(case.kind(), "type_case" | "default_case") {
        return None;
    }
    let mut cursor = case.walk();
    if !case
        .named_children(&mut cursor)
        .any(|node| node.kind() == "statement_list" && node.start_byte() <= offset)
    {
        return None;
    }

    let alias = switch.child_by_field_name("alias")?;
    let (_, name) = identifier_in_list(alias, name, text)?;
    let mut types = case
        .children_by_field_name("type", &mut cursor)
        .filter(Node::is_named);
    let explicit_type = types
        .next()
        .filter(|node| text.get(node.byte_range()) != Some("nil") && types.next().is_none());
    let initializer = if explicit_type.is_none() {
        switch.child_by_field_name("value")
    } else {
        None
    };
    Some(binding(name, explicit_type, initializer, 0))
}

fn declaration<'tree>(node: Node<'tree>, name: &str, text: &str) -> Option<Binding<'tree>> {
    let mut cursor = node.walk();
    match node.kind() {
        "var_declaration"
        | "const_declaration"
        | "type_declaration"
        | "var_spec_list"
        | "parameter_list"
        | "type_parameter_list"
        | "labeled_statement" => node
            .named_children(&mut cursor)
            .find_map(|child| declaration(child, name, text)),
        "short_var_declaration" => {
            let names = node.child_by_field_name("left")?;
            let (index, name) = identifier_in_list(names, name, text)?;
            let (initializer, result_index) =
                initializer_at(node.child_by_field_name("right"), index);
            Some(binding(name, None, initializer, result_index))
        }
        "type_spec"
        | "type_alias"
        | "var_spec"
        | "const_spec"
        | "parameter_declaration"
        | "variadic_parameter_declaration"
        | "type_parameter_declaration" => {
            let (index, name) = node
                .children_by_field_name("name", &mut cursor)
                .filter(Node::is_named)
                .enumerate()
                .find(|(_, candidate)| text.get(candidate.byte_range()) == Some(name))?;
            let (initializer, result_index) =
                initializer_at(node.child_by_field_name("value"), index);
            Some(binding(
                name,
                node.child_by_field_name("type"),
                initializer,
                result_index,
            ))
        }
        _ => None,
    }
}

fn initializer_at(list: Option<Node<'_>>, index: usize) -> (Option<Node<'_>>, usize) {
    let Some(list) = list else {
        return (None, 0);
    };
    let mut cursor = list.walk();
    let mut values = list
        .named_children(&mut cursor)
        .filter(|node| !node.is_extra());
    let first = values.next();
    let Some(second) = values.next() else {
        return (first, index);
    };
    (
        match index {
            0 => first,
            1 => Some(second),
            _ => values.nth(index - 2),
        },
        0,
    )
}

fn left_binding<'tree>(node: Node<'tree>, name: &str, text: &str) -> Option<Binding<'tree>> {
    let (index, name) = identifier_in_list(node.child_by_field_name("left")?, name, text)?;
    let initializer = if node.kind() == "range_clause" {
        Some(node)
    } else {
        node.child_by_field_name("right")
    };
    Some(binding(name, None, initializer, index))
}

fn identifier_in_list<'tree>(
    list: Node<'tree>,
    name: &str,
    text: &str,
) -> Option<(usize, Node<'tree>)> {
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter(|node| !node.is_extra())
        .enumerate()
        .find(|(_, node)| {
            matches!(
                node.kind(),
                "identifier" | "nil" | "true" | "false" | "iota"
            ) && text.get(node.byte_range()) == Some(name)
        })
}

fn has_short_operator(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|child| child.kind() == ":=")
}

fn binding<'tree>(
    name: Node<'tree>,
    explicit_type: Option<Node<'tree>>,
    initializer: Option<Node<'tree>>,
    result_index: usize,
) -> Binding<'tree> {
    Binding {
        name,
        explicit_type,
        initializer,
        result_index,
    }
}
