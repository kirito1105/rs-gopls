use std::collections::HashSet;

use super::*;
use crate::scope::lexical_binding;
use crate::semantic::resolver::{
    MAX_TYPE_DEPTH, TypeIdentity, TypeKind, explicit_type_arguments, is_predeclared_type,
    materialize_type, untyped_default,
};
use crate::semantic::source::{function_at, import_path};
use crate::symbols::{self, Container, OutlineKind, SymbolKind};

type Ty<'w> = TypeIdentity<'w>;

fn sdk_placeholder(ty: &Ty<'_>) -> bool {
    ty.package.id == "unsafe"
        && matches!(&ty.kind, TypeKind::Named { name, .. } if matches!(name.as_str(), "ArbitraryType" | "IntegerType"))
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Shape {
    Bool,
    Numeric,
    String,
    Pointer,
    Struct,
    Array,
    Slice,
    Map,
    Channel,
    Function,
    Interface,
}

fn shape<'w>(resolver: &mut Resolver<'w, '_>, ty: &Ty<'w>) -> Result<Option<Shape>> {
    if sdk_placeholder(ty) {
        return Ok(None);
    }
    let Some(base) = resolver.underlying_type(ty.clone())? else {
        return Ok(None);
    };
    if base.pointers > 0 {
        return Ok(Some(Shape::Pointer));
    }
    Ok(Some(match &base.kind {
        TypeKind::Named { name, .. } => {
            if let Some((file, details)) = resolver.type_info(base.package, name)? {
                if details.interface {
                    Shape::Interface
                } else if resolver.indexes.reference_source(&file)?.is_some_and(|s| {
                    s.as_ref()
                        .outline()
                        .symbols
                        .iter()
                        .any(|sym| sym.name == *name && sym.kind == OutlineKind::Struct)
                }) {
                    Shape::Struct
                } else {
                    return Ok(None);
                }
            } else {
                match name.as_str() {
                    "bool" => Shape::Bool,
                    "string" => Shape::String,
                    "any" | "error" | "comparable" => Shape::Interface,
                    n if is_predeclared_type(n) => Shape::Numeric,
                    _ => return Ok(None),
                }
            }
        }
        TypeKind::Inline { shape, .. } if shape.interface => Shape::Interface,
        TypeKind::Inline { .. } => Shape::Struct,
        TypeKind::Function { .. } => Shape::Function,
        TypeKind::Container(c, _) => match **c {
            Container::Array { .. } => Shape::Array,
            Container::Slice(_) => Shape::Slice,
            Container::Map { .. } => Shape::Map,
            Container::Channel { .. } => Shape::Channel,
        },
        TypeKind::Parameter(_) => return Ok(None),
    }))
}

impl<'s, 'w> Analysis<'s, 'w, '_> {
    fn universe(&self, node: Node<'_>) -> bool {
        let name = self.text(node);
        self.locals.lookup(node, name).is_none() && !self.locals.globals.contains_key(name)
    }

    pub(super) fn inferred(&mut self, node: Node<'_>) -> Result<Option<Ty<'w>>> {
        Ok(self
            .inferred_at(unparen(node), 0)?
            .filter(|ty| !sdk_placeholder(ty)))
    }

    fn inferred_at(&mut self, node: Node<'_>, depth: usize) -> Result<Option<Ty<'w>>> {
        if depth >= MAX_TYPE_DEPTH {
            return Ok(None);
        }
        let node = unparen(node);
        let name = self.text(node);
        if declarations::is_identifier(node)
            && let Some(local) = self.locals.lookup(node, name)
        {
            let Some(binding) =
                lexical_binding(node, name, &self.source.text).filter(|b| b.name == local.name)
            else {
                return Ok(None);
            };
            let owner = binding.name.parent().map(|p| p.kind());
            if !matches!(
                owner,
                Some("type_spec" | "type_alias" | "type_parameter_declaration" | "type_elem")
            ) {
                if let Some(syntax) = binding.explicit_type {
                    let ty = self.resolver.source_type(self.package, self.source, syntax);
                    return Ok(ty.map(|mut ty| {
                        if owner == Some("variadic_parameter_declaration") {
                            ty = Ty::new(
                                self.package,
                                TypeKind::Container(Box::new(Container::Slice(ty)), None),
                                0,
                            );
                        }
                        ty.addressable = owner != Some("const_spec");
                        ty
                    }));
                }
                if owner == Some("const_spec") {
                    // An untyped constant takes its type from the assignment context.
                    return Ok(None);
                }
                if binding.result_index == 0
                    && let Some(value) = binding.initializer
                {
                    let mut ty = self.inferred_at(value, depth + 1)?;
                    if let Some(ty) = &mut ty {
                        ty.addressable = true;
                    }
                    return Ok(ty);
                }
            }
        }
        if let Some(default) = untyped_default(node)
            && (!matches!(node.kind(), "true" | "false") || self.universe(node))
            && !self.locals.globals.contains_key(default)
        {
            return Ok(Some(Ty::named(self.package, default)));
        }
        if matches!(node.kind(), "index_expression" | "slice_expression")
            && let Some(operand) = node.child_by_field_name("operand")
            && let Some(ty) = self.inferred_at(operand, depth + 1)?
            && !ty.type_expression
            && !matches!(ty.kind, TypeKind::Function { .. })
        {
            let Some((container, pointers)) = self.resolver.container(ty.clone())? else {
                return Ok(None);
            };
            if node.kind() == "index_expression" {
                return self.resolver.indexed_element(ty);
            }
            return Ok(match (container, pointers) {
                (Container::Slice(_), 0) => Some(ty),
                (Container::Array { element, .. }, 0 | 1) => Some(Ty::new(
                    ty.package,
                    TypeKind::Container(Box::new(Container::Slice(element)), None),
                    0,
                )),
                _ => None,
            });
        }
        if node.kind() == "call_expression"
            && let Some(callee) = node.child_by_field_name("function")
            && callee.kind() == "identifier"
            && self.universe(callee)
        {
            let name = self.text(callee);
            if is_predeclared_type(name) {
                return Ok(Some(Ty::named(self.package, name)));
            }
            if matches!(name, "len" | "cap" | "copy") && !self.locals.globals.contains_key("int") {
                return Ok(Some(Ty::named(self.package, "int")));
            }
        }
        if node.kind() == "type_conversion_expression"
            && let Some(mut target) = node
                .child_by_field_name("type")
                .filter(|t| t.kind() == "pointer_type")
        {
            // A leading unparenthesized star dereferences the conversion result.
            let mut stars = 0;
            while target.kind() == "pointer_type" {
                let Some(inner) = target.named_child(0) else {
                    return Ok(None);
                };
                target = inner;
                stars += 1;
            }
            let Some(mut ty) = self.resolver.source_type(self.package, self.source, target) else {
                return Ok(None);
            };
            for _ in 0..stars {
                if ty.pointers == 0 {
                    let Some(base) = self.resolver.underlying_type(ty)? else {
                        return Ok(None);
                    };
                    ty = base;
                }
                let Some(pointers) = ty.pointers.checked_sub(1) else {
                    return Ok(None);
                };
                ty.pointers = pointers;
            }
            ty.addressable = true;
            return Ok(Some(ty));
        }
        let resolved = self
            .resolver
            .infer_type(self.package, self.source, node, depth, 0)?;
        if resolved.is_some() || node.kind() != "identifier" {
            return Ok(resolved);
        }
        if self.locals.globals.get(name) == Some(&OutlineKind::Variable)
            && let Some(symbol) = self.source.declarations().symbols.declaration(name)
            && let SymbolKind::InferredValue {
                initializer,
                result_index: 0,
            } = &symbol.kind
            && let Some(value) = self
                .source
                .tree
                .root_node()
                .named_descendant_for_byte_range(initializer.start, initializer.end)
        {
            return self.inferred_at(value, depth + 1);
        }
        if let Some((file, _)) = self.resolver.indexes.find_dot_import(
            self.resolver.workspace,
            self.package,
            &self.source.imports,
            name,
        )? && let Some(pkg) = self.resolver.workspace.package_of_file(&file)
            && let Some(symbol) = self.resolver.find_symbol(pkg, name, None)?
        {
            return self.resolver.symbol_type(self.source, &symbol, name, depth);
        }
        Ok(None)
    }
}

pub(super) fn literal_key(node: Node<'_>) -> Option<Node<'_>> {
    let wrapper = node.parent()?;
    let key = wrapper.parent()?;
    (wrapper.kind() == "literal_element"
        && key.kind() == "keyed_element"
        && key.child_by_field_name("key") == Some(wrapper))
    .then_some(key)
}

fn literal_owner<'w>(analysis: &mut Analysis<'_, 'w, '_>, key: Node<'_>) -> Option<Ty<'w>> {
    let literal = key.parent()?.parent()?;
    if literal.kind() != "composite_literal" {
        return None;
    }
    analysis.resolver.source_type(
        analysis.package,
        analysis.source,
        literal.child_by_field_name("type")?,
    )
}

fn key_shape(analysis: &mut Analysis<'_, '_, '_>, node: Node<'_>) -> Result<Option<Shape>> {
    let Some(ty) = literal_key(node).and_then(|key| literal_owner(analysis, key)) else {
        return Ok(None);
    };
    shape(&mut analysis.resolver, &ty)
}

pub(super) fn is_struct_key(analysis: &mut Analysis<'_, '_, '_>, node: Node<'_>) -> Result<bool> {
    Ok(key_shape(analysis, node)? == Some(Shape::Struct))
}

pub(super) fn is_container_key(
    analysis: &mut Analysis<'_, '_, '_>,
    node: Node<'_>,
) -> Result<bool> {
    Ok(matches!(
        key_shape(analysis, node)?,
        Some(Shape::Map | Shape::Array | Shape::Slice)
    ))
}

pub(super) fn check(analysis: &mut Analysis<'_, '_, '_>, node: Node<'_>) -> Result<()> {
    match node.kind() {
        "call_expression" | "type_conversion_expression" => check_call(analysis, node)?,
        "return_statement" => check_return(analysis, node)?,
        "var_spec" | "assignment_statement" | "short_var_declaration" => {
            check_assignment(analysis, node)?
        }
        "if_statement" | "for_clause" | "for_statement" => {
            let condition = node.child_by_field_name("condition").or_else(|| {
                (node.kind() == "for_statement")
                    .then(|| {
                        children(node)
                            .into_iter()
                            .find(|n| !matches!(n.kind(), "for_clause" | "range_clause" | "block"))
                    })
                    .flatten()
            });
            if let Some(cond) = condition
                && let Some(ty) = analysis.inferred(cond)?
                && let Some(shape) = shape(&mut analysis.resolver, &ty)?
                && !matches!(shape, Shape::Bool | Shape::Interface)
            {
                analysis.error(
                    DiagnosticCode::ConditionNotBool,
                    cond,
                    "Condition must have boolean type",
                );
            }
        }
        "keyed_element" => {
            if let Some(name) = node
                .child_by_field_name("key")
                .and_then(|key| key.named_child(0))
                && name.kind() == "identifier"
                && let Some(ty) = literal_owner(analysis, node)
                && shape(&mut analysis.resolver, &ty)? == Some(Shape::Struct)
                && !analysis
                    .resolver
                    .member(ty.package, &ty, analysis.text(name), false)?
                    .is_some_and(|s| matches!(s.symbol.kind, SymbolKind::Value(_)))
            {
                analysis.error(
                    DiagnosticCode::CompositeLiteralUnknownField,
                    name,
                    format!("Unknown struct field {}", analysis.text(name)),
                );
            }
        }
        "expression_switch_statement" => duplicate_cases(analysis, node),
        "identifier"
            if analysis.text(node) == "_"
                && unused::value_name(analysis, node)
                && unused::assignment_target(node).is_none() =>
        {
            analysis.error(
                DiagnosticCode::BlankAsValue,
                node,
                "Cannot use _ as a value",
            );
        }
        _ => {}
    }
    Ok(())
}

fn duplicate_cases(analysis: &mut Analysis<'_, '_, '_>, switch: Node<'_>) {
    let mut seen = HashSet::new();
    for case in children(switch)
        .into_iter()
        .filter(|n| n.kind() == "expression_case")
    {
        for expr in case
            .child_by_field_name("value")
            .map(children)
            .unwrap_or_default()
        {
            let mut literal = unparen(expr);
            let mut negative = false;
            if literal.kind() == "unary_expression" {
                match literal.child_by_field_name("operator").map(|op| op.kind()) {
                    Some("-") => negative = true,
                    Some("+") => {}
                    _ => continue,
                }
                let Some(operand) = literal.child_by_field_name("operand") else {
                    continue;
                };
                literal = unparen(operand);
            }
            let spelling = analysis.text(literal);
            let (kind, value) = match literal.kind() {
                "int_literal" => {
                    let digits = spelling.replace('_', "").to_ascii_lowercase();
                    let (radix, digits) = if let Some(s) = digits.strip_prefix("0x") {
                        (16, s)
                    } else if let Some(s) = digits.strip_prefix("0b") {
                        (2, s)
                    } else if let Some(s) = digits.strip_prefix("0o") {
                        (8, s)
                    } else if digits.starts_with('0') {
                        (8, digits.as_str())
                    } else {
                        (10, digits.as_str())
                    };
                    (
                        "int",
                        u128::from_str_radix(digits, radix)
                            .map_or_else(|_| spelling.to_owned(), |n| n.to_string()),
                    )
                }
                "raw_string_literal" | "interpreted_string_literal" => {
                    match import_path(spelling) {
                        Some(value) => ("string", value),
                        None => ("quoted string", spelling.to_owned()),
                    }
                }
                "float_literal" | "rune_literal" => (literal.kind(), spelling.to_owned()),
                _ => continue,
            };
            if !seen.insert((kind, negative && value != "0", value)) {
                analysis.error(
                    DiagnosticCode::DuplicateSwitchCase,
                    expr,
                    format!("Duplicate switch case {}", analysis.text(expr)),
                );
            }
        }
    }
}

struct Signature<'w> {
    parameters: Vec<Option<Ty<'w>>>,
    results: Vec<Option<Ty<'w>>>,
    variadic: bool,
}

fn signature<'w>(analysis: &mut Analysis<'_, 'w, '_>, ty: Ty<'w>) -> Result<Option<Signature<'w>>> {
    let TypeKind::Function {
        file,
        signature,
        mut substitutions,
        method_receiver,
    } = ty.kind
    else {
        return Ok(None);
    };
    for param in &signature.type_parameters {
        if !substitutions.iter().any(|(name, _)| name == &param.name) {
            substitutions.push((param.name.clone(), None));
        }
    }
    let parsed;
    let params = if signature.parameters.is_empty() && signature.origin.function_type().is_none() {
        let Some(source) = analysis.resolver.indexes.reference_source(&file)? else {
            return Ok(None);
        };
        let source = source.as_ref();
        let Some(list) = function_at(source, signature.origin.range())
            .and_then(|f| f.child_by_field_name("parameters"))
        else {
            return Ok(None);
        };
        parsed = symbols::parameter_types(list, |n| {
            symbols::type_ref(n, &source.text, &|n| source.range(n))
        });
        &parsed
    } else {
        &signature.parameters
    };
    let imports = &analysis.resolver.indexes.file_index(&file)?.imports;
    let resolve = |reference: &Option<symbols::TypeRef>| {
        materialize_type(
            analysis.resolver.workspace,
            ty.package,
            &file,
            imports,
            reference.clone()?,
            &substitutions,
        )
    };
    Ok(Some(Signature {
        parameters: method_receiver
            .into_iter()
            .map(|t| Some(*t))
            .chain(params.iter().map(resolve))
            .collect(),
        results: signature.results.iter().map(resolve).collect(),
        variadic: signature.variadic,
    }))
}

enum Call<'w> {
    Function(Signature<'w>),
    Builtin(&'static str),
    Conversion,
    NonFunction,
}

fn call_target<'w>(
    analysis: &mut Analysis<'_, 'w, '_>,
    call: Node<'_>,
) -> Result<Option<Call<'w>>> {
    let (callee, explicit, indexed) = if let Some(function) = call.child_by_field_name("function") {
        (
            unparen(function),
            call.child_by_field_name("type_arguments"),
            false,
        )
    } else if let Some(ty) = call.child_by_field_name("type") {
        let ty = unparen(ty);
        if ty.kind() == "generic_type" {
            let Some(base) = ty.child_by_field_name("type") else {
                return Ok(None);
            };
            (base, ty.child_by_field_name("type_arguments"), true)
        } else {
            return Ok(Some(Call::Conversion));
        }
    } else {
        return Ok(None);
    };
    if callee.kind() == "identifier" && analysis.universe(callee) {
        let name = analysis.text(callee);
        if is_predeclared_type(name) {
            return Ok(Some(Call::Conversion));
        }
        if let Some(builtin) = [
            "len", "cap", "delete", "copy", "close", "append", "make", "new", "clear", "complex",
            "imag", "real", "min", "max", "panic", "print", "println", "recover",
        ]
        .into_iter()
        .find(|b| *b == name)
        {
            return Ok(Some(Call::Builtin(builtin)));
        }
    }
    let Some(mut ty) = analysis.inferred(callee)? else {
        return Ok(None);
    };
    if ty.type_expression {
        return Ok(Some(Call::Conversion));
    }
    if let Some(list) = explicit {
        let args = explicit_type_arguments(list);
        let specialized = if matches!(&ty.kind, TypeKind::Function { signature, .. } if !signature.type_parameters.is_empty())
        {
            analysis
                .resolver
                .specialize_function(analysis.package, analysis.source, ty, args)?
        } else if indexed && args.len() == 1 {
            analysis.resolver.indexed_element(ty)?
        } else {
            None
        };
        let Some(t) = specialized else {
            return Ok(None);
        };
        ty = t;
    }
    let Some(base) = analysis.resolver.underlying_type(ty)? else {
        return Ok(None);
    };
    if base.pointers == 0 && matches!(base.kind, TypeKind::Function { .. }) {
        return Ok(signature(analysis, base)?.map(Call::Function));
    }
    Ok(shape(&mut analysis.resolver, &base)?
        .filter(|s| *s != Shape::Interface)
        .map(|_| Call::NonFunction))
}

fn argument_nodes(call: Node<'_>) -> Vec<Node<'_>> {
    call.child_by_field_name("arguments")
        .map(children)
        .unwrap_or_else(|| call.child_by_field_name("operand").into_iter().collect())
}

enum Value<'t, 'w> {
    Expression(Node<'t>),
    CallResult(Node<'t>, Option<Ty<'w>>),
}

impl<'t, 'w> Value<'t, 'w> {
    fn node(&self) -> Node<'t> {
        match self {
            Self::Expression(n) | Self::CallResult(n, _) => *n,
        }
    }

    fn check(&self, analysis: &mut Analysis<'_, 'w, '_>, expected: &Ty<'w>) -> Result<()> {
        match self {
            Self::Expression(n) => check_value(analysis, expected, *n),
            Self::CallResult(n, Some(actual)) => check_types(analysis, expected, actual, *n, false),
            Self::CallResult(_, None) => Ok(()),
        }
    }
}

fn supplied<'t, 'w>(
    analysis: &mut Analysis<'_, 'w, '_>,
    values: &[Node<'t>],
) -> Result<Option<Vec<Value<'t, 'w>>>> {
    if let [expr] = values
        && matches!(
            unparen(*expr).kind(),
            "call_expression" | "type_conversion_expression"
        )
    {
        match call_target(analysis, unparen(*expr))? {
            Some(Call::Function(signature)) if signature.results.len() != 1 => {
                return Ok(Some(
                    signature
                        .results
                        .into_iter()
                        .map(|ty| Value::CallResult(*expr, ty))
                        .collect(),
                ));
            }
            Some(Call::Builtin("delete" | "close" | "clear" | "print" | "println" | "panic")) => {
                return Ok(Some(vec![]));
            }
            None | Some(Call::NonFunction) => return Ok(None),
            _ => {}
        }
    }
    Ok(Some(
        values.iter().copied().map(Value::Expression).collect(),
    ))
}

fn check_call(analysis: &mut Analysis<'_, '_, '_>, call: Node<'_>) -> Result<()> {
    let Some(target) = call_target(analysis, call)? else {
        return Ok(());
    };
    let callee = call
        .child_by_field_name("function")
        .or_else(|| call.child_by_field_name("type"))
        .unwrap_or(call);
    if matches!(target, Call::NonFunction) {
        analysis.error(
            DiagnosticCode::CallNonFunction,
            callee,
            format!("Cannot call non-function {}", analysis.text(callee)),
        );
        return Ok(());
    }
    let args = argument_nodes(call);
    let Some(values) = supplied(analysis, &args)? else {
        return Ok(());
    };
    let count = values.len();
    let spread = args.last().is_some_and(|n| n.kind() == "variadic_argument");
    let (min, max) = match &target {
        Call::Function(sig) if sig.variadic && !spread => {
            (sig.parameters.len().saturating_sub(1), usize::MAX)
        }
        Call::Function(sig) => (sig.parameters.len(), sig.parameters.len()),
        Call::Builtin("copy" | "delete" | "complex") => (2, 2),
        Call::Builtin("append") if spread => (2, 2),
        Call::Builtin("append" | "min" | "max") => (1, usize::MAX),
        Call::Builtin("print" | "println") => (0, usize::MAX),
        Call::Builtin("recover") => (0, 0),
        Call::Builtin("make") => {
            let ty = args.first().and_then(|n| {
                analysis
                    .resolver
                    .source_type(analysis.package, analysis.source, *n)
            });
            match ty
                .as_ref()
                .map(|t| shape(&mut analysis.resolver, t))
                .transpose()?
                .flatten()
            {
                Some(Shape::Slice) => (2, 3),
                Some(Shape::Channel | Shape::Map) => (1, 2),
                _ => (1, 3),
            }
        }
        _ => (1, 1),
    };
    if !(min..=max).contains(&count)
        || matches!(&target, Call::Function(sig) if spread && !sig.variadic)
    {
        let expected = if min == max {
            min.to_string()
        } else if max == usize::MAX {
            format!("at least {min}")
        } else {
            format!("{min} to {max}")
        };
        analysis.error(
            DiagnosticCode::CallArity,
            call.child_by_field_name("arguments").unwrap_or(call),
            format!(
                "{} expects {expected} arguments, got {count}",
                analysis.text(callee)
            ),
        );
    } else if let Call::Function(sig) = target {
        for (i, arg) in values.iter().enumerate() {
            let index = i.min(sig.parameters.len().saturating_sub(1));
            if let Some(Some(ty)) = sig.parameters.get(index) {
                if arg.node().kind() == "variadic_argument" {
                    if let Some(slice) = arg.node().named_child(0) {
                        let expected = Ty::new(
                            ty.package,
                            TypeKind::Container(Box::new(Container::Slice(ty.clone())), None),
                            0,
                        );
                        check_value(analysis, &expected, slice)?;
                    }
                } else {
                    arg.check(analysis, ty)?;
                }
            }
        }
    }
    Ok(())
}

fn check_return(analysis: &mut Analysis<'_, '_, '_>, stmt: Node<'_>) -> Result<()> {
    let Some(function) = std::iter::successors(stmt.parent(), |n| n.parent()).find(|n| {
        matches!(
            n.kind(),
            "func_literal" | "function_declaration" | "method_declaration"
        )
    }) else {
        return Ok(());
    };
    let mut results = Vec::new();
    let mut named = true;
    if let Some(result) = function.child_by_field_name("result") {
        if result.kind() == "parameter_list" {
            for param in children(result) {
                let names = field_children(param, "name").len();
                named &= names > 0;
                results.extend(std::iter::repeat_n(
                    param.child_by_field_name("type"),
                    names.max(1),
                ));
            }
        } else {
            named = false;
            results.push(Some(result));
        }
    }
    let exprs = children(stmt)
        .into_iter()
        .find(|n| n.kind() == "expression_list")
        .map(children)
        .unwrap_or_default();
    if exprs.is_empty() && named {
        return Ok(());
    }
    let Some(values) = supplied(analysis, &exprs)? else {
        return Ok(());
    };
    let count = values.len();
    if count != results.len() {
        analysis.error(
            DiagnosticCode::ReturnArity,
            stmt,
            format!("Expected {} return values, got {count}", results.len()),
        );
    } else {
        for (ty, value) in results.into_iter().zip(values) {
            if let Some(ty) = ty.and_then(|n| {
                analysis
                    .resolver
                    .source_type(analysis.package, analysis.source, n)
            }) {
                value.check(analysis, &ty)?;
            }
        }
    }
    Ok(())
}

fn check_assignment(analysis: &mut Analysis<'_, '_, '_>, node: Node<'_>) -> Result<()> {
    let var = node.kind() == "var_spec";
    if node.kind() == "assignment_statement" && token(node, "=").is_none() {
        return Ok(());
    }
    let Some(rhs) = node.child_by_field_name(if var { "value" } else { "right" }) else {
        return Ok(());
    };
    let lhs = if var {
        field_children(node, "name")
    } else {
        node.child_by_field_name("left")
            .map(children)
            .unwrap_or_default()
    };
    let annotation = node.child_by_field_name("type");
    let expressions = children(rhs);
    if var && annotation.is_none() && !expressions.iter().any(|v| unparen(*v).kind() == "nil") {
        return Ok(());
    }
    let Some(values) = supplied(analysis, &expressions)? else {
        return Ok(());
    };
    if lhs.len() != values.len() {
        return Ok(());
    }
    for (name, value) in lhs.into_iter().zip(values) {
        let existing = !var
            && (node.kind() == "assignment_statement"
                || analysis
                    .locals
                    .lookup(name, analysis.text(name))
                    .is_some_and(|binding| {
                        analysis.locals.declared_in_same_block(
                            node,
                            analysis.text(name),
                            binding.name,
                        )
                    }));
        let nil = unparen(value.node());
        if !existing && annotation.is_none() && nil.kind() == "nil" && analysis.universe(nil) {
            analysis.error(
                DiagnosticCode::NilWithoutType,
                nil,
                "Cannot infer a type from nil",
            );
            continue;
        }
        let expected = if let Some(ty) = annotation {
            analysis
                .resolver
                .source_type(analysis.package, analysis.source, ty)
        } else if existing && analysis.text(name) != "_" {
            analysis.inferred(name)?
        } else {
            None
        };
        if let Some(expected) = expected {
            value.check(analysis, &expected)?;
        }
    }
    Ok(())
}

fn check_value<'w>(
    analysis: &mut Analysis<'_, 'w, '_>,
    expected: &Ty<'w>,
    value: Node<'_>,
) -> Result<()> {
    let Some(actual) = analysis.inferred(value)? else {
        return Ok(());
    };
    let untyped = untyped_default(value).is_some()
        && (!matches!(unparen(value).kind(), "true" | "false")
            || analysis.universe(unparen(value)));
    check_types(analysis, expected, &actual, value, untyped)
}

fn check_types<'w>(
    analysis: &mut Analysis<'_, 'w, '_>,
    expected: &Ty<'w>,
    actual: &Ty<'w>,
    value: Node<'_>,
    untyped: bool,
) -> Result<()> {
    let (Some(left), Some(right)) = (
        shape(&mut analysis.resolver, expected)?,
        shape(&mut analysis.resolver, actual)?,
    ) else {
        return Ok(());
    };
    if left == Shape::Interface {
        if right != Shape::Interface
            && let Some(missing) = missing_method(analysis, expected, actual)?
        {
            analysis.error(
                DiagnosticCode::InterfaceNotImplemented,
                value,
                format!("Value does not implement the interface (method {missing})"),
            );
        }
    } else if right != Shape::Interface {
        if left != right
            || (!untyped && distinct_types(&mut analysis.resolver, expected, actual, 0)?)
        {
            analysis.error(
                DiagnosticCode::TypeMismatch,
                value,
                format!(
                    "Cannot use {} as {}",
                    analysis.text(value),
                    type_name(expected)
                ),
            );
        }
    }
    Ok(())
}

fn type_name(ty: &Ty<'_>) -> String {
    let name = match &ty.kind {
        TypeKind::Named { name, .. } => name.as_str(),
        TypeKind::Inline { shape, .. } if shape.interface => "interface",
        TypeKind::Inline { .. } => "struct",
        TypeKind::Function { .. } => "function",
        TypeKind::Container(c, _) => match **c {
            Container::Array { .. } => "array",
            Container::Slice(_) => "slice",
            Container::Channel { .. } => "channel",
            Container::Map { .. } => "map",
        },
        TypeKind::Parameter(_) => "type parameter",
    };
    format!("{}{name}", "*".repeat(ty.pointers.into()))
}

fn universe_name(name: &str) -> &str {
    match name {
        "byte" => "uint8",
        "rune" => "int32",
        _ => name,
    }
}

fn distinct_types<'w>(
    resolver: &mut Resolver<'w, '_>,
    left: &Ty<'w>,
    right: &Ty<'w>,
    depth: usize,
) -> Result<bool> {
    if depth >= MAX_TYPE_DEPTH || sdk_placeholder(left) || sdk_placeholder(right) {
        return Ok(false);
    }
    let (Some(left), Some(right)) = (
        resolver.resolve_alias(left.clone())?,
        resolver.resolve_alias(right.clone())?,
    ) else {
        return Ok(false);
    };
    if let (
        TypeKind::Named {
            name: l,
            arguments: la,
        },
        TypeKind::Named {
            name: r,
            arguments: ra,
        },
    ) = (&left.kind, &right.kind)
    {
        let l_defined = resolver.type_info(left.package, l)?.is_some();
        let r_defined = resolver.type_info(right.package, r)?.is_some();
        if !(l_defined || is_predeclared_type(l)) || !(r_defined || is_predeclared_type(r)) {
            return Ok(false);
        }
        if left.pointers == right.pointers {
            if l_defined != r_defined {
                return Ok(true);
            }
            if !l_defined {
                return Ok(universe_name(l) != universe_name(r));
            }
            if l != r || left.package.directory_and_name() != right.package.directory_and_name() {
                return Ok(true);
            }
            for (l, r) in la.iter().zip(ra) {
                if let (Some(l), Some(r)) = (l, r)
                    && distinct_types(resolver, l, r, depth + 1)?
                {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
    }
    let (Some(l), Some(r)) = (
        resolver.underlying_type(left)?,
        resolver.underlying_type(right)?,
    ) else {
        return Ok(false);
    };
    if l.pointers != r.pointers {
        return Ok(shape(resolver, &l)?.is_some() && shape(resolver, &r)?.is_some());
    }
    match (&l.kind, &r.kind) {
        (TypeKind::Container(l, _), TypeKind::Container(r, _)) => Ok(match (&**l, &**r) {
            (Container::Array { element: l, .. }, Container::Array { element: r, .. })
            | (Container::Slice(l), Container::Slice(r))
            | (Container::Channel { element: l, .. }, Container::Channel { element: r, .. }) => {
                distinct_types(resolver, l, r, depth + 1)?
            }
            (Container::Map { key: lk, value: lv }, Container::Map { key: rk, value: rv }) => {
                distinct_types(resolver, lk, rk, depth + 1)?
                    || distinct_types(resolver, lv, rv, depth + 1)?
            }
            _ => true,
        }),
        _ => Ok(false),
    }
}

fn missing_method<'w>(
    analysis: &mut Analysis<'_, 'w, '_>,
    interface: &Ty<'w>,
    actual: &Ty<'w>,
) -> Result<Option<String>> {
    let mut pending = vec![(interface.clone(), 0)];
    let mut visited = 0;
    while let Some((interface, depth)) = pending.pop() {
        visited += 1;
        if depth >= MAX_TYPE_DEPTH || visited > MAX_TYPE_DEPTH * MAX_TYPE_DEPTH {
            return Ok(None);
        }
        let Some((file, mut details, substitutions)) =
            analysis.resolver.type_context(&interface)?
        else {
            if matches!(&interface.kind, TypeKind::Named { name, .. } if name == "error")
                && let Some(problem) = method_problem(analysis, &interface, actual, "Error")?
            {
                return Ok(Some(problem));
            }
            continue;
        };
        details.method_names.sort_unstable();
        for name in details.method_names {
            if let Some(problem) = method_problem(analysis, &interface, actual, &name)? {
                return Ok(Some(problem));
            }
        }
        for base in details
            .alias
            .into_iter()
            .chain(details.underlying)
            .chain(details.embedded)
        {
            let imports = &analysis.resolver.indexes.file_index(&file)?.imports;
            if let Some(ty) = materialize_type(
                analysis.resolver.workspace,
                interface.package,
                &file,
                imports,
                base,
                &substitutions,
            ) {
                pending.push((ty, depth + 1));
            }
        }
    }
    Ok(None)
}

fn method_problem<'w>(
    analysis: &mut Analysis<'_, 'w, '_>,
    interface: &Ty<'w>,
    actual: &Ty<'w>,
    name: &str,
) -> Result<Option<String>> {
    let mut receiver = actual.clone();
    receiver.addressable = false;
    let Some(method) = analysis
        .resolver
        .member(interface.package, &receiver, name, true)?
    else {
        if unresolved::definitely_missing(&mut analysis.resolver, &receiver, name)? {
            return Ok(Some(name.into()));
        }
        receiver.addressable = true;
        return Ok(analysis
            .resolver
            .member(interface.package, &receiver, name, true)?
            .filter(|s| s.symbol.pointer_receiver && actual.pointers == 0)
            .map(|_| name.into()));
    };
    let SymbolKind::Function(got) = &method.symbol.kind else {
        return Ok(Some(name.into()));
    };
    let Some(required) = analysis
        .resolver
        .member(interface.package, interface, name, true)?
    else {
        if name != "Error" {
            return Ok(None);
        }
        if !got.parameters.is_empty() || got.results.len() != 1 {
            return Ok(Some(name.into()));
        }
        if let Some(ty) = analysis
            .resolver
            .symbol_type(analysis.source, &method, name, 0)?
            && let Some(signature) = signature(analysis, ty)?
            && let Some(Some(result)) = signature.results.first()
            && let Some(actual) = analysis.resolver.resolve_alias(result.clone())?
        {
            let valid = matches!(&actual.kind, TypeKind::Named { name, .. } if name == "string")
                && actual.pointers == 0
                && analysis
                    .resolver
                    .type_info(actual.package, "string")?
                    .is_none();
            if !valid && shape(&mut analysis.resolver, &actual)?.is_some() {
                return Ok(Some(name.into()));
            }
        }
        return Ok(None);
    };
    let SymbolKind::Function(want) = &required.symbol.kind else {
        return Ok(None);
    };
    if want.variadic != got.variadic
        || want.parameters.len() != got.parameters.len()
        || want.results.len() != got.results.len()
    {
        return Ok(Some(name.into()));
    }
    let left = analysis
        .resolver
        .symbol_type(analysis.source, &required, name, 0)?;
    let right = analysis
        .resolver
        .symbol_type(analysis.source, &method, name, 0)?;
    if let (Some(left), Some(right)) = (left, right)
        && let (Some(left), Some(right)) = (signature(analysis, left)?, signature(analysis, right)?)
    {
        for (l, r) in left
            .parameters
            .iter()
            .chain(&left.results)
            .zip(right.parameters.iter().chain(&right.results))
        {
            if let (Some(l), Some(r)) = (l, r)
                && (distinct_types(&mut analysis.resolver, l, r, 0)?
                    || matches!((shape(&mut analysis.resolver, l)?, shape(&mut analysis.resolver, r)?), (Some(left), Some(right)) if left != right))
            {
                return Ok(Some(name.into()));
            }
        }
    }
    Ok(None)
}
