use std::collections::HashSet;

use super::*;
use crate::semantic::source::is_declaration_name;

pub(super) fn assignment_target(mut node: Node<'_>) -> Option<Node<'_>> {
    while let Some(parent) = node
        .parent()
        .filter(|p| p.kind() == "parenthesized_expression")
    {
        node = parent;
    }
    let list = node.parent()?;
    let stmt = list.parent()?;
    (list.kind() == "expression_list" && stmt.child_by_field_name("left") == Some(list))
        .then_some(stmt)
}

pub(super) fn value_name(analysis: &Analysis<'_, '_, '_>, node: Node<'_>) -> bool {
    !analysis.locals.declarations.contains(&node.id())
        && !is_declaration_name(node)
        && !node.parent().is_some_and(|p| {
            matches!(p.kind(), "package_clause" | "import_spec")
                || (p.kind() == "qualified_type" && p.child_by_field_name("name") == Some(node))
        })
}

pub(super) fn collect(analysis: &mut Analysis<'_, '_, '_>) -> Result<()> {
    if analysis.locals.variables.is_empty() {
        return Ok(());
    }
    let mut used = HashSet::new();
    walk(analysis.source.tree.root_node(), |node| {
        if declarations::is_identifier(node)
            && value_name(analysis, node)
            && let Some(binding) = analysis.locals.lookup(node, analysis.text(node))
            && !assignment_target(node).is_some_and(|stmt| {
                stmt.kind() != "assignment_statement" || token(stmt, "=").is_some()
            })
            && !types::is_struct_key(analysis, node)?
        {
            used.insert(binding.name.id());
        }
        Ok(())
    })?;
    for node in std::mem::take(&mut analysis.locals.variables) {
        if !used.contains(&node.id()) {
            let name = analysis.text(node);
            let item = analysis.error(
                DiagnosticCode::VarUnused,
                node,
                format!("{name} declared and unused"),
            );
            item.tags.push(DiagnosticTag::Unnecessary);
            item.fix_data = DiagnosticFixData::ReplaceUnusedVarWithBlank {
                name: name.to_owned(),
                ident_range: item.range,
            };
        }
    }
    Ok(())
}
