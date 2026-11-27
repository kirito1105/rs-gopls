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
            "communication_case" => case_entry_binding(child, name, text),
            "function_declaration" | "method_declaration" | "func_literal"
                if child.kind() == "block" =>
            {
                function_binding(scope, name, text)
            }
            "function_declaration" => type_parameter_binding(scope, offset, name, text),
            "method_declaration" => receiver_type_parameter_binding(scope, name, text),
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
    if binding.is_some()
        && let Some(initial) = case_entry_binding(statement_list, name, text)
    {
        return Some(initial);
    }
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

// The implicit block of a select or type-switch case already contains its
// receive variables or guard variable before the first statement is reached.
fn case_entry_binding<'tree>(
    statements: Node<'tree>,
    name: &str,
    text: &str,
) -> Option<Binding<'tree>> {
    if statements.kind() != "statement_list" {
        return None;
    }
    let case = statements.parent()?;
    match case.kind() {
        "communication_case" => case
            .child_by_field_name("communication")
            .filter(|n| n.kind() == "receive_statement" && has_short_operator(*n))
            .and_then(|receive| left_binding(receive, name, text)),
        "type_case" | "default_case" => {
            let switch = case
                .parent()
                .filter(|s| s.kind() == "type_switch_statement")?;
            type_case_binding(switch, case, statements.start_byte(), name, text)
        }
        _ => None,
    }
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
        .or_else(|| receiver_type_parameter_binding(function, name, text))
}

pub(crate) fn receiver_type_parameters(method: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
    let receiver = method.child_by_field_name("receiver")?;
    let mut ty = receiver.named_child(0)?.child_by_field_name("type")?;
    if ty.kind() == "pointer_type" {
        ty = ty.named_child(0)?;
    }
    (ty.kind() == "generic_type").then(|| {
        Some((
            ty.child_by_field_name("type")?,
            ty.child_by_field_name("type_arguments")?,
        ))
    })?
}

fn receiver_type_parameter_binding<'tree>(
    method: Node<'tree>,
    name: &str,
    text: &str,
) -> Option<Binding<'tree>> {
    let (_, arguments) = receiver_type_parameters(method)?;
    let mut cursor = arguments.walk();
    let binding_name = arguments
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "type_elem")
        .filter_map(|node| node.named_child(0))
        .find(|node| {
            node.kind() == "type_identifier" && text.get(node.byte_range()) == Some(name)
        })?;
    Some(binding(binding_name, None, None, 0))
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

pub(crate) fn reference_binding<'tree>(node: Node<'tree>, text: &str) -> Option<Node<'tree>> {
    let spelling = text.get(node.byte_range())?;
    if spelling == "_" {
        return None;
    }
    let previous = lexical_binding(node, spelling, text).map(|binding| binding.name);
    if declaration_binding(node, text).is_none() {
        return previous;
    }
    let parent = node.parent()?;
    if parent.kind() == "expression_list"
        && let Some(statement) = parent
            .parent()
            .filter(|node| node.kind() == "short_var_declaration")
        && let Some(block) = statement
            .parent()
            .filter(|node| node.kind() == "statement_list")
        && let Some(previous) = previous
        && same_short_declaration_scope(previous, block, spelling, text)
    {
        return Some(previous);
    }
    Some(node)
}

fn same_short_declaration_scope(
    previous: Node<'_>,
    block: Node<'_>,
    spelling: &str,
    text: &str,
) -> bool {
    if case_entry_binding(block, spelling, text).is_some_and(|binding| binding.name == previous) {
        return true;
    }
    let Some(body) = block.parent() else {
        return false;
    };
    let mut cursor = previous.parent();
    while let Some(node) = cursor {
        match node.kind() {
            "statement_list" => return node == block,
            "function_declaration" | "method_declaration" | "func_literal" => {
                return node.child_by_field_name("body") == Some(body);
            }
            "source_file" => return false,
            _ => cursor = node.parent(),
        }
    }
    false
}

pub(crate) fn declaration_binding<'tree>(name: Node<'tree>, text: &str) -> Option<Binding<'tree>> {
    let parent = name.parent()?;
    let owner = if parent.kind() == "expression_list" {
        parent.parent()?
    } else {
        parent
    };
    let spelling = text.get(name.byte_range())?;
    let binding = match owner.kind() {
        "range_clause" | "receive_statement" if has_short_operator(owner) => {
            left_binding(owner, spelling, text)
        }
        "type_switch_statement" if parent.kind() == "expression_list" => {
            let (_, name) =
                identifier_in_list(owner.child_by_field_name("alias")?, spelling, text)?;
            Some(binding(name, None, owner.child_by_field_name("value"), 0))
        }
        _ => declaration(owner, spelling, text),
    }?;
    (binding.name == name).then_some(binding)
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
        | "type_parameter_declaration"
        | "field_declaration" => {
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

pub(crate) fn initializer_at(list: Option<Node<'_>>, index: usize) -> (Option<Node<'_>>, usize) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_scope_short_declarations_reuse_the_guard_or_receive_variable() {
        for (text, first_case, second_case) in [
            (
                "package p\nfunc f(a any) { switch value := a.(type) { case int: ptr := &value; value, n := value+1, 2; _, _, _ = ptr, value, n; case string: _ = value } }",
                "ptr :=",
                "case string",
            ),
            (
                "package p\nfunc f(c chan int) { select { case value := <-c: ptr := &value; value, n := value+1, 2; _, _, _ = ptr, value, n; case <-c: value := 8; _ = value } }",
                "ptr :=",
                "case <-c",
            ),
        ] {
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_go::LANGUAGE.into())
                .unwrap();
            let tree = parser.parse(text, None).unwrap();
            assert!(!tree.root_node().has_error());
            let guard = text.find("value :=").unwrap();
            let start = text.find(first_case).unwrap();
            let end = text.find(second_case).unwrap();
            let uses: Vec<_> = text[start..end]
                .match_indices("value")
                .map(|(index, _)| start + index)
                .collect();
            assert_eq!(uses.len(), 4);
            for at in uses {
                let node = tree
                    .root_node()
                    .named_descendant_for_byte_range(at, at + 1)
                    .unwrap();
                assert_eq!(
                    reference_binding(node, text).unwrap().start_byte(),
                    guard,
                    "use at byte {at} in {text}"
                );
            }
            let last = text.rfind("value").unwrap();
            let node = tree
                .root_node()
                .named_descendant_for_byte_range(last, last + 1)
                .unwrap();
            let expected = if second_case == "case string" {
                guard
            } else {
                text.rfind("value :=").unwrap()
            };
            assert_eq!(
                reference_binding(node, text).unwrap().start_byte(),
                expected
            );
        }
    }
}
