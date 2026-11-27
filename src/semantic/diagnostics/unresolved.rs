use super::*;
use crate::semantic::resolver::{
    MAX_TYPE_DEPTH, TypeIdentity, TypeKind, is_exported, is_predeclared, is_predeclared_type,
    materialize_type,
};
use crate::symbols::OutlineKind;

fn type_annotation(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| match parent.kind() {
        "var_spec"
        | "const_spec"
        | "type_spec"
        | "type_alias"
        | "parameter_declaration"
        | "variadic_parameter_declaration"
        | "field_declaration"
        | "type_assertion_expression"
        | "composite_literal" => parent.child_by_field_name("type") == Some(node),
        "function_declaration"
        | "method_declaration"
        | "func_literal"
        | "function_type"
        | "method_elem" => parent.child_by_field_name("result") == Some(node),
        _ => false,
    })
}

pub(super) fn check(analysis: &mut Analysis<'_, '_, '_>, node: Node<'_>) -> Result<()> {
    if matches!(node.kind(), "qualified_type" | "selector_expression") {
        return selector(analysis, node);
    }
    if !declarations::is_identifier(node) || !unused::value_name(analysis, node) {
        return Ok(());
    }
    let name = analysis.text(node);
    if node.kind() == "type_identifier"
        && type_annotation(node)
        && !analysis.locals.incomplete_package
    {
        let actual_type = if let Some(local) = analysis.locals.lookup(node, name) {
            Some(local.name.parent().is_some_and(|n| {
                matches!(
                    n.kind(),
                    "type_alias" | "type_spec" | "type_parameter_declaration" | "type_elem"
                )
            }))
        } else if let Some(kind) = analysis.locals.globals.get(name) {
            Some(matches!(
                kind,
                OutlineKind::Struct | OutlineKind::Interface | OutlineKind::Class
            ))
        } else {
            is_predeclared(name).then(|| is_predeclared_type(name))
        };
        if actual_type == Some(false) {
            analysis.error(
                DiagnosticCode::TypeUnresolved,
                node,
                format!("{name} is not a type"),
            );
            return Ok(());
        }
    }
    if is_predeclared(name)
        || name == "_"
        || analysis.locals.globals.contains_key(name)
        || analysis.locals.incomplete_package
        || analysis.locals.lookup(node, name).is_some()
    {
        return Ok(());
    }
    if types::literal_key(node).is_some() && !types::is_container_key(analysis, node)? {
        return Ok(());
    }
    let qualifier = imports::qualifier(node);
    for import in &analysis.source.imports {
        let pkg = analysis
            .resolver
            .workspace
            .resolve_import(analysis.package, &import.path);
        if import
            .alias
            .as_deref()
            .or_else(|| pkg.map(|p| p.name.as_str()))
            == Some(name)
            || (qualifier && pkg.is_none() && import.alias.is_none())
            || (import.alias.as_deref() == Some(".")
                && is_exported(name)
                && match pkg {
                    Some(pkg) if !pkg.load_error => {
                        analysis.resolver.find_symbol(pkg, name, None)?.is_some()
                    }
                    _ => true,
                })
        {
            return Ok(());
        }
    }
    let is_type = node.kind() == "type_identifier" && !qualifier;
    // A Go-escaped import path may not have an entry in the navigation index.
    if (qualifier || is_exported(name))
        && imports::specs(analysis.source.tree.root_node())
            .into_iter()
            .any(|spec| {
                let alias = spec.child_by_field_name("name").map(|n| analysis.text(n));
                let could_bind = (qualifier && alias.is_none_or(|s| s == name))
                    || (is_exported(name) && alias == Some("."));
                could_bind
                    && spec.child_by_field_name("path").is_some_and(|n| {
                        crate::semantic::source::import_path(analysis.text(n)).is_none()
                    })
            })
    {
        return Ok(());
    }
    let code = if is_type {
        DiagnosticCode::TypeUnresolved
    } else {
        DiagnosticCode::NameUnresolved
    };
    let item = analysis.error(
        code,
        node,
        format!("Undefined {}{name}", if is_type { "type " } else { "" }),
    );
    if qualifier {
        item.fix_data = DiagnosticFixData::ImportMissingPackage {
            qualifier: name.to_owned(),
            ident_range: item.range,
        };
    }
    Ok(())
}

fn selector(analysis: &mut Analysis<'_, '_, '_>, node: Node<'_>) -> Result<()> {
    let qualified = node.kind() == "qualified_type";
    let (Some(receiver), Some(field)) = (
        node.child_by_field_name(if qualified { "package" } else { "operand" }),
        node.child_by_field_name(if qualified { "name" } else { "field" }),
    ) else {
        return Ok(());
    };
    let name = analysis.text(field);
    if declarations::is_identifier(receiver)
        && analysis
            .locals
            .lookup(receiver, analysis.text(receiver))
            .is_none()
        && let Some((_, package)) = analysis.source.imported_package(
            analysis.resolver.workspace,
            analysis.package,
            analysis.text(receiver),
        )
    {
        if package.load_error {
            return Ok(());
        }
        let symbol = analysis.resolver.find_symbol(package, name, None)?;
        if !is_exported(name)
            && symbol.is_some()
            && package.directory_and_name() != analysis.package.directory_and_name()
        {
            analysis.error(
                DiagnosticCode::NameUnexported,
                field,
                format!("{name} is not exported by {}", package.name),
            );
        } else if symbol.is_none() {
            analysis.error(
                if qualified {
                    DiagnosticCode::TypeUnresolved
                } else {
                    DiagnosticCode::NameUnresolved
                },
                field,
                format!("Undefined {}.{name}", analysis.text(receiver)),
            );
        }
        return Ok(());
    }
    if qualified {
        return Ok(());
    }
    let Some(ty) = analysis.inferred(receiver)? else {
        return Ok(());
    };
    if analysis
        .resolver
        .member(analysis.package, &ty, name, true)?
        .is_some()
    {
        return Ok(());
    }
    if !is_exported(name)
        && let Some(owner) = analysis.resolver.resolve_alias(ty.clone())?
        && owner.package.directory_and_name() != analysis.package.directory_and_name()
        && analysis
            .resolver
            .member(owner.package, &owner, name, true)?
            .is_some()
    {
        analysis.error(
            DiagnosticCode::NameUnexported,
            field,
            format!("Cannot refer to unexported member {name}"),
        );
    } else if definitely_missing(&mut analysis.resolver, &ty, name)? {
        analysis.error(
            DiagnosticCode::FieldOrMethodUnresolved,
            field,
            format!("No field or method named {name}"),
        );
    }
    Ok(())
}

// A failed navigation lookup can also mean an incomplete embedding or ambiguous promotion.
pub(super) fn definitely_missing<'w>(
    resolver: &mut Resolver<'w, '_>,
    ty: &TypeIdentity<'w>,
    name: &str,
) -> Result<bool> {
    let mut pending = vec![(ty.clone(), 0)];
    let mut visited = 0;
    while let Some((ty, depth)) = pending.pop() {
        visited += 1;
        if visited > MAX_TYPE_DEPTH * MAX_TYPE_DEPTH
            || depth >= MAX_TYPE_DEPTH
            || ty.package.load_error
            || ty.pointers > 1
            || resolver.direct_member(&ty, name, false)?.is_some()
        {
            return Ok(false);
        }
        let Some((file, info, substitutions)) = resolver.type_context(&ty)? else {
            match &ty.kind {
                TypeKind::Named { name: base, .. }
                    if !is_predeclared_type(base)
                        || matches!(base.as_str(), "any" | "comparable")
                        || (base == "error" && name == "Error") =>
                {
                    return Ok(false);
                }
                TypeKind::Parameter(_) => return Ok(false),
                _ => continue,
            }
        };
        if info.has_type_terms
            || matches!(&ty.kind, TypeKind::Inline { shape, .. } if shape.has_type_terms)
        {
            return Ok(false);
        }
        for base in info
            .alias
            .into_iter()
            .chain(info.underlying)
            .chain(info.embedded)
        {
            let imports = &resolver.indexes.file_index(&file)?.imports;
            let Some(base) = materialize_type(
                resolver.workspace,
                ty.package,
                &file,
                imports,
                base,
                &substitutions,
            ) else {
                return Ok(false);
            };
            pending.push((base, depth + 1));
        }
    }
    Ok(true)
}
