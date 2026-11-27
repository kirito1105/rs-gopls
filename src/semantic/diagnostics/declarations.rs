use std::collections::{HashMap, HashSet};

use super::*;
use crate::scope::receiver_type_parameters;
use crate::symbols::OutlineKind;
use crate::workspace::path_key;

#[derive(Clone, Copy)]
pub(super) struct Local<'t> {
    pub(super) name: Node<'t>,
    visible_from: usize,
}

#[derive(Default)]
pub(super) struct Locals<'t> {
    bindings: HashMap<(usize, &'t str), Local<'t>>,
    pub(super) declarations: HashSet<usize>,
    pub(super) variables: Vec<Node<'t>>,
    pub(super) globals: HashMap<String, OutlineKind>,
    pub(super) incomplete_package: bool,
}

impl<'t> Locals<'t> {
    pub(super) fn declared_in_same_block(
        &self,
        node: Node<'_>,
        name: &str,
        previous: Node<'_>,
    ) -> bool {
        block_scope(node)
            .and_then(|scope| self.bindings.get(&(scope.id(), name)))
            .is_some_and(|b| b.name == previous)
    }

    pub(super) fn lookup(&self, node: Node<'_>, name: &str) -> Option<Local<'t>> {
        std::iter::successors(node.parent(), |n| n.parent()).find_map(|scope| {
            self.bindings
                .get(&(scope.id(), name))
                .filter(|v| v.visible_from <= node.start_byte())
                .copied()
        })
    }
}

pub(super) fn collect<'s>(analysis: &mut Analysis<'s, '_, '_>) -> Result<()> {
    package_declarations(analysis)?;
    walk(analysis.source.tree.root_node(), |node| {
        match node.kind() {
            "var_spec" | "const_spec" | "type_spec" | "type_alias" => {
                if let Some(scope) = block_scope(node) {
                    for name in field_children(node, "name") {
                        let start = if matches!(node.kind(), "type_spec" | "type_alias") {
                            name.start_byte()
                        } else {
                            node.end_byte()
                        };
                        bind(analysis, scope, name, start, node.kind() == "var_spec");
                    }
                }
            }
            "parameter_declaration"
            | "variadic_parameter_declaration"
            | "type_parameter_declaration" => {
                if let Some(owner) = node.parent().and_then(|list| list.parent()) {
                    let start = if node.kind() == "type_parameter_declaration" {
                        owner.start_byte()
                    } else {
                        owner
                            .child_by_field_name("body")
                            .map_or(owner.end_byte(), |n| n.start_byte())
                    };
                    for name in field_children(node, "name") {
                        bind(analysis, owner, name, start, false);
                    }
                }
            }
            "method_declaration" => {
                if let Some((_, arguments)) = receiver_type_parameters(node) {
                    for arg in children(arguments)
                        .into_iter()
                        .filter_map(|n| n.named_child(0))
                        .filter(|n| n.kind() == "type_identifier")
                    {
                        bind(analysis, node, arg, node.start_byte(), false);
                    }
                }
            }
            "short_var_declaration" | "receive_statement" | "range_clause" => {
                if let (Some(operator), Some(left), Some(scope)) = (
                    token(node, ":="),
                    node.child_by_field_name("left"),
                    block_scope(node),
                ) {
                    let mut distinct = HashSet::new();
                    let mut new = false;
                    for name in children(left) {
                        let spelling = analysis.text(name);
                        analysis.locals.declarations.insert(name.id());
                        if spelling == "_" {
                            continue;
                        }
                        if !is_identifier(name) {
                            return Ok(());
                        }
                        if !distinct.insert(spelling) {
                            redeclared(analysis, name);
                        } else if !analysis
                            .locals
                            .bindings
                            .contains_key(&(scope.id(), spelling))
                        {
                            bind(analysis, scope, name, node.end_byte(), true);
                            new = true;
                        }
                    }
                    if !new {
                        let item = analysis.error(
                            DiagnosticCode::ShortVarNoNewVars,
                            operator,
                            "No new variables on the left of :=",
                        );
                        item.fix_data = DiagnosticFixData::ChangeShortDeclToAssign {
                            operator_range: item.range,
                        };
                    }
                }
            }
            "type_switch_statement" => {
                if let Some(alias) = node
                    .child_by_field_name("alias")
                    .and_then(|n| n.named_child(0))
                    && analysis.text(alias) != "_"
                {
                    analysis.locals.declarations.insert(alias.id());
                    analysis.locals.variables.push(alias);
                    for case in children(node)
                        .into_iter()
                        .filter(|n| matches!(n.kind(), "type_case" | "default_case"))
                    {
                        let start = children(case)
                            .into_iter()
                            .find(|n| n.kind() == "statement_list")
                            .map_or(case.end_byte(), |n| n.start_byte());
                        bind(analysis, case, alias, start, false);
                    }
                }
            }
            "field_declaration_list" | "interface_type" => member_declarations(analysis, node),
            _ => {}
        }
        Ok(())
    })?;
    walk(analysis.source.tree.root_node(), |node| {
        let operands = match node.kind() {
            "assignment_statement" | "receive_statement" | "range_clause"
                if token(node, ":=").is_none() =>
            {
                node.child_by_field_name("left")
                    .map(children)
                    .unwrap_or_default()
            }
            "inc_statement" | "dec_statement" => children(node),
            _ => return Ok(()),
        };
        for operand in operands {
            let operand = unparen(operand);
            let name = analysis.text(operand);
            let constant = if is_identifier(operand) {
                if let Some(local) = analysis.locals.lookup(operand, name) {
                    local
                        .name
                        .parent()
                        .is_some_and(|p| p.kind() == "const_spec")
                } else if let Some(kind) = analysis.locals.globals.get(name) {
                    *kind == OutlineKind::Constant
                } else {
                    analysis
                        .resolver
                        .indexes
                        .find_dot_import(
                            analysis.resolver.workspace,
                            analysis.package,
                            &analysis.source.imports,
                            name,
                        )?
                        .and_then(|(path, _)| analysis.resolver.workspace.package_of_file(&path))
                        .map(|pkg| analysis.resolver.find_symbol(pkg, name, None))
                        .transpose()?
                        .flatten()
                        .is_some_and(|s| {
                            matches!(s.symbol.kind, crate::symbols::SymbolKind::Value(_))
                                && !s.symbol.addressable
                        })
                }
            } else if operand.kind() == "selector_expression"
                && let (Some(receiver), Some(field)) = (
                    operand.child_by_field_name("operand"),
                    operand.child_by_field_name("field"),
                )
                && let Some((_, package)) = analysis.source.imported_package(
                    analysis.resolver.workspace,
                    analysis.package,
                    analysis.text(receiver),
                )
                && analysis
                    .locals
                    .lookup(receiver, analysis.text(receiver))
                    .is_none()
            {
                analysis
                    .resolver
                    .find_symbol(package, analysis.text(field), None)?
                    .is_some_and(|s| {
                        matches!(s.symbol.kind, crate::symbols::SymbolKind::Value(_))
                            && !s.symbol.addressable
                    })
            } else {
                false
            };
            if constant {
                analysis.error(
                    DiagnosticCode::ConstReassign,
                    operand,
                    format!("Cannot assign to constant {name}"),
                );
            }
        }
        Ok(())
    })
}

pub(super) fn is_identifier(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "identifier" | "type_identifier" | "package_identifier" | "nil" | "true" | "false" | "iota"
    )
}

fn block_scope(node: Node<'_>) -> Option<Node<'_>> {
    for scope in std::iter::successors(node.parent(), |n| n.parent()) {
        match scope.kind() {
            "block" => {
                if let Some(function) = scope.parent().filter(|n| {
                    matches!(
                        n.kind(),
                        "function_declaration" | "method_declaration" | "func_literal"
                    )
                }) {
                    return Some(function);
                }
                return Some(scope);
            }
            "if_statement"
            | "for_statement"
            | "expression_switch_statement"
            | "type_switch_statement"
            | "type_case"
            | "expression_case"
            | "default_case"
            | "communication_case" => return Some(scope),
            _ => {}
        }
    }
    None
}

fn bind<'s>(
    analysis: &mut Analysis<'s, '_, '_>,
    scope: Node<'s>,
    name: Node<'s>,
    visible_from: usize,
    variable: bool,
) {
    let spelling = analysis.text(name);
    analysis.locals.declarations.insert(name.id());
    if spelling == "_" {
        return;
    }
    if let std::collections::hash_map::Entry::Vacant(entry) =
        analysis.locals.bindings.entry((scope.id(), spelling))
    {
        entry.insert(Local { name, visible_from });
        if variable {
            analysis.locals.variables.push(name);
        }
    } else {
        redeclared(analysis, name);
    }
}

fn redeclared(analysis: &mut Analysis<'_, '_, '_>, name: Node<'_>) {
    analysis.error(
        DiagnosticCode::NameRedeclared,
        name,
        format!("{} redeclared in this scope", analysis.text(name)),
    );
}

fn member_declarations(analysis: &mut Analysis<'_, '_, '_>, list: Node<'_>) {
    let mut seen = HashSet::new();
    for field in children(list) {
        let mut names = field_children(field, "name");
        if names.is_empty()
            && field.kind() == "field_declaration"
            && let Some(mut ty) = field.child_by_field_name("type")
        {
            if ty.kind() == "generic_type"
                && let Some(base) = ty.child_by_field_name("type")
            {
                ty = base;
            }
            names.push(ty.child_by_field_name("name").unwrap_or(ty));
        }
        for name in names {
            let spelling = analysis.text(name);
            if spelling != "_" && !seen.insert(spelling) {
                redeclared(analysis, name);
            }
        }
    }
}

fn package_declarations(analysis: &mut Analysis<'_, '_, '_>) -> Result<()> {
    let key = path_key(&analysis.source.path);
    let mut seen = HashSet::new();
    let mut fields = HashSet::new();
    let mut files = analysis
        .resolver
        .indexes
        .files(analysis.package)
        .into_owned();
    files.sort_by_cached_key(|path| path_key(path));
    for file in &files {
        let other;
        let path = path_key(file);
        let source = if path == key {
            analysis.source
        } else {
            let Some(found) = analysis.resolver.indexes.reference_source(file)? else {
                analysis.locals.incomplete_package = true;
                continue;
            };
            other = found;
            other.as_ref()
        };
        if !syntax::has_reliable_declarations(source.tree.root_node())
            || source.outline().package.as_deref() != Some(analysis.package.name.as_str())
        {
            analysis.locals.incomplete_package = true;
            continue;
        }
        for symbol in &source.outline().symbols {
            let owner = symbol.receiver.as_ref().map(|r| r.base.clone());
            if owner.is_none() {
                analysis
                    .locals
                    .globals
                    .insert(symbol.name.clone(), symbol.kind);
            }
            if path < key {
                seen.insert((owner, symbol.name.clone()));
            }
            if symbol.kind == OutlineKind::Struct {
                fields.extend(
                    symbol
                        .children()
                        .iter()
                        .filter(|s| s.kind == OutlineKind::Field)
                        .map(|field| (Some(symbol.name.clone()), field.name.clone())),
                );
            }
        }
    }
    for symbol in &analysis.source.outline().symbols {
        if symbol.name == "_" || (symbol.name == "init" && symbol.kind == OutlineKind::Function) {
            continue;
        }
        let member = (
            symbol.receiver.as_ref().map(|r| r.base.clone()),
            symbol.name.clone(),
        );
        if fields.contains(&member) || !seen.insert(member) {
            let range = analysis.source.byte_range(symbol.selection_range.clone());
            analysis.items.push(DiagnosticItem::new(
                DiagnosticCode::NameRedeclared,
                DiagnosticSeverity::Error,
                range,
                format!("{} redeclared in this package", symbol.name),
            ));
        }
    }
    Ok(())
}
