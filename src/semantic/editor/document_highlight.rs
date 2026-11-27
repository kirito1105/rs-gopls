use std::cmp::Reverse;

use lsp_types::{DocumentHighlight, DocumentHighlightKind, Position, Range};
use tree_sitter::Node;

use super::{
    Editor, Occurrence, SymbolId, SymbolKind, ancestors, descendants, function_owner, is_breakable,
    is_function, named_children, parameter_slots,
};
use crate::scope::initializer_at;

pub(super) fn highlights(editor: &Editor<'_>, position: Position) -> Vec<DocumentHighlight> {
    let Some(leaf) = caret_leaf(editor, position) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    let mut add = |node, kind| {
        let range = editor.range(node);
        if range.start < range.end {
            result.push(DocumentHighlight {
                range,
                kind: Some(kind),
            });
        }
    };

    if matches!(leaf.kind(), "func" | "return") || is_panic_callee(editor, leaf) {
        if let Some(owner) = function_owner(leaf) {
            function_exits(editor, owner, |node| add(node, DocumentHighlightKind::TEXT));
        }
    } else if matches!(
        leaf.kind(),
        "for" | "switch" | "select" | "break" | "continue"
    ) {
        if let Some(target) = jump_target_at(editor, leaf) {
            jump_occurrences(editor, target, |node| {
                add(node, DocumentHighlightKind::TEXT)
            });
        }
    } else {
        if let Some(owner) = function_owner(leaf) {
            return_occurrences(owner, leaf, |node| add(node, DocumentHighlightKind::TEXT));
        }
        if leaf.kind() != "dot"
            && let Some(symbol) = editor.symbol(leaf)
        {
            let spelling = editor.text(leaf);
            for node in descendants(editor.root()) {
                if node.child_count() == 0
                    && editor.text(node) == spelling
                    && let Some(candidate) = editor.symbol(node)
                    && candidate.id == symbol.id
                {
                    add(node, access(node, &candidate));
                }
            }
            result.extend(implicit_import_highlight(editor, spelling, symbol));
        }
    }

    result.sort_by_key(|item| {
        let priority = match item.kind {
            Some(DocumentHighlightKind::WRITE) => 2,
            Some(DocumentHighlightKind::READ) => 1,
            _ => 0,
        };
        (item.range.start, item.range.end, Reverse(priority))
    });
    result.dedup_by_key(|item| item.range);
    result
}

fn implicit_import_highlight(
    editor: &Editor<'_>,
    spelling: &str,
    symbol: Occurrence<'_>,
) -> Option<DocumentHighlight> {
    let binding = editor.imports.get(spelling).copied().flatten()?;
    if symbol.id != SymbolId::Declaration(binding.start_byte())
        || binding.parent()?.child_by_field_name("path") != Some(binding)
    {
        return None;
    }
    // The path text inside the quotes is the declaration range for an implicit import.
    let range = Range::new(
        editor.position(binding.start_byte() + 1),
        editor.position(binding.end_byte() - 1),
    );
    Some(DocumentHighlight {
        range,
        kind: Some(DocumentHighlightKind::TEXT),
    })
}

fn caret_leaf<'tree>(editor: &Editor<'tree>, position: Position) -> Option<Node<'tree>> {
    let offset = editor.offset(position)?;
    let text = editor.source();
    let root = editor.root();
    let char_at = |offset| text.get(offset..)?.chars().next();
    let locate = |offset| {
        let end = offset + char_at(offset)?.len_utf8();
        root.descendant_for_byte_range(offset, end)
    };

    let current = locate(offset);
    if current.is_some_and(in_comment) {
        return None;
    }
    if !char_at(offset).is_some_and(caret_character)
        && let Some((previous, _)) = text.get(..offset)?.char_indices().next_back()
        && char_at(previous).is_some_and(caret_character)
    {
        return locate(previous).filter(|node| !in_comment(*node));
    }
    current
}

fn caret_character(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '\'' | '"' | '`')
}

fn in_comment(node: Node<'_>) -> bool {
    ancestors(node).any(|node| node.kind() == "comment")
}

pub(super) fn access(node: Node<'_>, symbol: &Occurrence) -> DocumentHighlightKind {
    if !matches!(
        symbol.kind,
        SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::Property
    ) {
        return DocumentHighlightKind::TEXT;
    }

    let variable_definition = symbol.declaration
        && node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), "var_spec" | "const_spec"));
    let struct_key = symbol.kind == SymbolKind::Property
        && node
            .parent()
            .filter(|parent| parent.kind() == "literal_element")
            .is_some_and(|key| {
                key.parent().is_some_and(|element| {
                    element.kind() == "keyed_element"
                        && element.child_by_field_name("key") == Some(key)
                })
            });
    if variable_definition || struct_key || is_write_target(node) {
        DocumentHighlightKind::WRITE
    } else {
        DocumentHighlightKind::READ
    }
}

fn is_write_target(mut expression: Node<'_>) -> bool {
    while let Some(parent) = expression.parent() {
        match parent.kind() {
            "parenthesized_expression" => {}
            "unary_expression"
                if parent
                    .child_by_field_name("operator")
                    .is_some_and(|operator| matches!(operator.kind(), "*" | "&" | "<-")) => {}
            "selector_expression" if parent.child_by_field_name("field") == Some(expression) => {}
            "inc_statement" | "dec_statement" => return true,
            "send_statement" => return parent.child_by_field_name("channel") == Some(expression),
            "expression_list" => {
                return parent
                    .parent()
                    .is_some_and(|statement| match statement.kind() {
                        "assignment_statement"
                        | "short_var_declaration"
                        | "range_clause"
                        | "receive_statement" => {
                            statement.child_by_field_name("left") == Some(parent)
                        }
                        "type_switch_statement" => {
                            statement.child_by_field_name("alias") == Some(parent)
                        }
                        _ => false,
                    });
            }
            _ => return false,
        }
        expression = parent;
    }
    false
}

fn is_panic_callee(editor: &Editor<'_>, callee: Node<'_>) -> bool {
    callee.kind() == "identifier"
        && editor.text(callee) == "panic"
        && callee.parent().is_some_and(|call| {
            call.kind() == "call_expression" && call.child_by_field_name("function") == Some(callee)
        })
        && editor
            .symbol(callee)
            .is_some_and(|symbol| symbol.default_library)
}

fn function_exits<'tree>(
    editor: &Editor<'tree>,
    owner: Node<'tree>,
    mut add: impl FnMut(Node<'tree>),
) {
    if let Some(keyword) = keyword(owner, "func") {
        add(keyword);
    }
    let Some(body) = owner.child_by_field_name("body") else {
        return;
    };
    walk_function_body(body, |node| match node.kind() {
        "return_statement" => {
            add(node);
            false
        }
        "call_expression"
            if node
                .child_by_field_name("function")
                .is_some_and(|callee| is_panic_callee(editor, callee)) =>
        {
            add(node);
            true
        }
        _ => true,
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JumpTarget<'tree> {
    Statement(Node<'tree>),
    Label(Node<'tree>),
}

fn jump_target_at<'tree>(editor: &Editor<'tree>, leaf: Node<'tree>) -> Option<JumpTarget<'tree>> {
    let parent = leaf.parent()?;
    if is_breakable(parent) {
        Some(JumpTarget::Statement(parent))
    } else {
        jump_target(editor, parent)
    }
}

fn jump_target<'tree>(editor: &Editor<'tree>, jump: Node<'tree>) -> Option<JumpTarget<'tree>> {
    if !matches!(jump.kind(), "break_statement" | "continue_statement") {
        return None;
    }
    if let Some(label) = named_children(jump).find(|node| node.kind() == "label_name") {
        return editor.label_definition(label).map(JumpTarget::Label);
    }
    let allowed = |node: Node<'_>| {
        if jump.kind() == "continue_statement" {
            node.kind() == "for_statement"
        } else {
            is_breakable(node)
        }
    };
    ancestors(jump)
        .skip(1)
        .take_while(|node| !is_function(*node))
        .find(|node| allowed(*node))
        .map(JumpTarget::Statement)
}

fn jump_occurrences<'tree>(
    editor: &Editor<'tree>,
    target: JumpTarget<'tree>,
    mut add: impl FnMut(Node<'tree>),
) {
    let scope = match target {
        JumpTarget::Statement(statement) => {
            let keyword_name = match statement.kind() {
                "for_statement" => "for",
                "select_statement" => "select",
                _ => "switch",
            };
            if let Some(keyword) = keyword(statement, keyword_name) {
                add(keyword);
            }
            statement
        }
        JumpTarget::Label(label) => {
            add(label);
            function_owner(label)
                .and_then(|owner| owner.child_by_field_name("body"))
                .or_else(|| label.parent())
                .unwrap_or(label)
        }
    };
    walk_function_body(scope, |node| {
        if jump_target(editor, node) == Some(target) {
            add(node);
        }
        true
    });
}

struct ResultSlot<'tree> {
    name: Option<Node<'tree>>,
    ty: Option<Node<'tree>>,
}

fn result_slots(owner: Node<'_>) -> Vec<ResultSlot<'_>> {
    let Some(result) = owner.child_by_field_name("result") else {
        return Vec::new();
    };
    if result.kind() != "parameter_list" {
        return vec![ResultSlot {
            name: None,
            ty: Some(result),
        }];
    }
    parameter_slots(result)
        .map(|(parameter, name)| ResultSlot {
            name,
            ty: parameter.child_by_field_name("type"),
        })
        .collect()
}

fn return_occurrences<'tree>(
    owner: Node<'tree>,
    leaf: Node<'tree>,
    mut add: impl FnMut(Node<'tree>),
) {
    let Some(body) = owner.child_by_field_name("body") else {
        return;
    };
    let slots = result_slots(owner);
    let Some((index, signature)) = return_slot_at(leaf, owner, &slots) else {
        return;
    };
    if let Some(signature) = signature {
        add(signature);
    }
    walk_function_body(body, |node| {
        if node.kind() != "return_statement" {
            return true;
        }
        let list = named_children(node).find(|part| part.kind() == "expression_list");
        if let (Some(expression), _) = initializer_at(list, index) {
            add(expression);
        }
        false
    });
}

fn return_slot_at<'tree>(
    leaf: Node<'tree>,
    owner: Node<'tree>,
    slots: &[ResultSlot<'tree>],
) -> Option<(usize, Option<Node<'tree>>)> {
    for node in ancestors(leaf).take_while(|node| *node != owner) {
        if let Some(index) = slots
            .iter()
            .position(|slot| slot.name.or(slot.ty) == Some(node))
        {
            return Some((index, Some(node)));
        }
        if matches!(
            node.kind(),
            "block" | "parameter_list" | "argument_list" | "literal_value"
        ) {
            return None;
        }
        let parent = node.parent()?;
        if parent.kind() == "expression_list"
            && parent
                .parent()
                .is_some_and(|statement| statement.kind() == "return_statement")
        {
            let index = named_children(parent).position(|expression| expression == node)?;
            let slot = slots.get(index)?;
            return Some((index, slot.ty.or(slot.name)));
        }
    }
    None
}

fn keyword<'tree>(node: Node<'tree>, spelling: &str) -> Option<Node<'tree>> {
    node.children(&mut node.walk())
        .find(|child| child.kind() == spelling && !child.is_missing())
}

fn walk_function_body<'tree>(root: Node<'tree>, mut visit: impl FnMut(Node<'tree>) -> bool) {
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if node.is_named()
            && !node.is_extra()
            && !is_function(node)
            && visit(node)
            && cursor.goto_first_child()
        {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}
