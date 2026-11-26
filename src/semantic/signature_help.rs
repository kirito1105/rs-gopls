use std::fmt::Write;

use lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, SignatureHelp,
    SignatureInformation,
};

use super::presentation::{
    builtin_function, documentation, node_at_range, parameter_labels_with, result_label_with,
    syntax_text_with,
};
use super::*;

mod cursor;

use cursor::{Call, CallSite, call_at, repair_call};

struct Signature {
    name: Option<String>,
    parameters: Vec<String>,
    result: Option<String>,
    variadic: bool,
    documentation: String,
}

impl Signature {
    fn from_source(
        function: Node<'_>,
        text: &str,
        substitutions: &[(String, String)],
    ) -> Option<Self> {
        let list = function.child_by_field_name("parameters")?;
        let render = |ty| syntax_text_with(ty, text, substitutions);
        let variadic = list
            .named_children(&mut list.walk())
            .any(|node| node.kind() == "variadic_parameter_declaration");
        let owner = function
            .parent()
            .filter(|parent| function.kind() == "function_type" && is_type_declaration(*parent))
            .unwrap_or(function);
        Some(Self {
            name: function
                .child_by_field_name("name")
                .map(|node| node_text(node, text).into()),
            parameters: parameter_labels_with(list, text, render),
            result: result_label_with(function, text, render),
            variadic,
            documentation: documentation(owner, text),
        })
    }

    fn at(self, raw_parameter: u32) -> SignatureHelp {
        let mut label = String::from("func");
        if let Some(name) = self.name {
            let _ = write!(label, " {name}");
        }
        label.push('(');
        let mut offset = label.encode_utf16().count() as u32;
        let mut parameters = Vec::with_capacity(self.parameters.len());
        for parameter in self.parameters {
            if !parameters.is_empty() {
                label.push_str(", ");
                offset += 2;
            }
            let end = offset + parameter.encode_utf16().count() as u32;
            parameters.push(ParameterInformation {
                label: ParameterLabel::LabelOffsets([offset, end]),
                documentation: None,
            });
            label.push_str(&parameter);
            offset = end;
        }
        label.push(')');
        if let Some(result) = self.result {
            let _ = write!(label, " {result}");
        }
        let active_parameter = if parameters.is_empty() {
            None
        } else if self.variadic {
            Some(raw_parameter.min(parameters.len() as u32 - 1))
        } else {
            Some(raw_parameter)
        };
        let documentation = (!self.documentation.is_empty()).then_some(
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: self.documentation,
            }),
        );
        SignatureHelp {
            signatures: vec![SignatureInformation {
                label,
                documentation,
                parameters: Some(parameters),
                active_parameter: None,
            }],
            active_signature: Some(0),
            active_parameter,
        }
    }
}

impl Engine {
    pub fn signature_help(
        &mut self,
        path: &Path,
        position: Position,
    ) -> Result<QueryProgress<Option<SignatureHelp>>> {
        self.signature_help_query(path, position, true)
    }

    pub fn signature_help_after_load_failure(
        &mut self,
        path: &Path,
        position: Position,
    ) -> Result<Option<SignatureHelp>> {
        match self.signature_help_query(path, position, false)? {
            QueryProgress::Ready(signature) => Ok(signature),
            _ => unreachable!("signature help fallback uses only source syntax and builtins"),
        }
    }

    fn signature_help_query(
        &mut self,
        path: &Path,
        position: Position,
        load_workspace: bool,
    ) -> Result<QueryProgress<Option<SignatureHelp>>> {
        let disk;
        let original = if let Some(source) = self.documents.get(&path_key(path)) {
            source
        } else if path.extension().is_some_and(|extension| extension == "go") {
            disk = match read_source(&mut self.parser, path) {
                Ok(source) => source,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
                {
                    return Ok(QueryProgress::Ready(None));
                }
                Err(error) => return Err(error),
            };
            &disk
        } else {
            return Ok(QueryProgress::Ready(None));
        };
        let Some(offset) = original.lines.offset(&original.text, position) else {
            return Ok(QueryProgress::Ready(None));
        };
        let repaired;
        let (source, call) = match call_at(original, offset) {
            CallSite::Found(call) => (original, call),
            CallSite::Blocked => return Ok(QueryProgress::Ready(None)),
            CallSite::Missing => {
                repaired = repair_call(&mut self.parser, original, offset)?;
                let Some(repaired) = &repaired else {
                    return Ok(QueryProgress::Ready(None));
                };
                let CallSite::Found(call) = call_at(repaired, offset) else {
                    return Ok(QueryProgress::Ready(None));
                };
                (repaired, call)
            }
        };
        let ready = |signature: Option<Signature>| {
            QueryProgress::Ready(signature.map(|signature| signature.at(call.active_parameter)))
        };
        if call.type_arguments.is_none()
            && let Some(function) = local_callable(source, call.callee, 0)
            && (!load_workspace || function.child_by_field_name("type_parameters").is_none())
        {
            return Ok(ready(Signature::from_source(function, &source.text, &[])));
        }
        let builtin = unshadowed_builtin(source, call.callee);
        if !load_workspace {
            return Ok(ready(
                builtin
                    .map(|(_, syntax)| fallback_builtin(&mut self.parser, syntax))
                    .transpose()?
                    .flatten(),
            ));
        }

        let test_source = is_test_source(path);
        self.include_tests |= test_source;
        let load = self.workspace_load();
        let Some(workspace) = &self.workspace else {
            return Ok(QueryProgress::Pending(load));
        };
        let from = active_package(workspace, original, &self.new_documents);
        if (test_source && !workspace.includes_tests())
            || self.graph_changes.affects_source(workspace, from, original)
        {
            return Ok(QueryProgress::Pending(load));
        }
        let indexes = IndexReader {
            parser: &mut self.parser,
            documents: &self.documents,
            disk_index: &mut self.disk_index,
            drafts: draft_files(workspace, &self.documents, &self.new_documents),
        };
        let mut resolver = Resolver { workspace, indexes };
        if let Some(from) = from {
            if let Some(ty) = resolver.callable_type(from, source, call)? {
                return Ok(ready(resolver.present_callable(from, source, call, ty)?));
            }
            let callee = unparenthesize(call.callee);
            if callee.kind() == "selector_expression"
                && let (Some(receiver), Some(name)) = (
                    callee.child_by_field_name("operand"),
                    callee.child_by_field_name("field"),
                )
                && node_text(name, &source.text) == "Error"
                && resolver.is_builtin_error(from, source, unparenthesize(receiver))?
            {
                let mut signature =
                    resolver.builtin_signature(&self.root, "Error", "func Error() string")?;
                if let Some(signature) = &mut signature {
                    // The predeclared error type has no package-indexed symbol.
                    let receiver = unparenthesize(receiver);
                    let receiver_type = match resolver.infer_type(from, source, receiver, 0, 0)? {
                        Some(ty) if ty.type_expression => resolver.type_text(from, source, &ty)?,
                        None if node_text(receiver, &source.text) == "error" => {
                            Some("error".into())
                        }
                        _ => None,
                    };
                    if let Some(receiver_type) = receiver_type {
                        signature.parameters.insert(0, receiver_type);
                    }
                }
                return Ok(ready(signature));
            }
            if builtin.is_some()
                && resolver
                    .source_name_location(from, source, callee)?
                    .is_some()
            {
                return Ok(ready(None));
            }
        }
        match builtin {
            Some((name, syntax)) => {
                Ok(ready(resolver.builtin_signature(&self.root, name, syntax)?))
            }
            None => Ok(ready(None)),
        }
    }
}

fn function_at(source: &Source, origin: Range) -> Option<Node<'_>> {
    let node = node_at_range(source, origin)?.parent()?;
    matches!(
        node.kind(),
        "function_declaration"
            | "method_declaration"
            | "method_elem"
            | "function_type"
            | "func_literal"
    )
    .then_some(node)
}

fn unparenthesize(mut node: Node<'_>) -> Node<'_> {
    while matches!(
        node.kind(),
        "parenthesized_expression" | "parenthesized_type"
    ) {
        let Some(child) = node
            .named_children(&mut node.walk())
            .find(|child| !child.is_extra())
        else {
            break;
        };
        node = child;
    }
    node
}

fn local_name<'tree>(source: &'tree Source, name: Node<'tree>) -> Option<Node<'tree>> {
    let spelling = node_text(name, &source.text);
    if let Some(binding) = lexical_binding(name, spelling, &source.text) {
        return Some(binding.name);
    }
    let symbol = source.declarations().symbols.declaration(spelling)?;
    node_at_range(source, symbol.range)
}

fn local_callable<'tree>(
    source: &'tree Source,
    callee: Node<'tree>,
    depth: usize,
) -> Option<Node<'tree>> {
    if depth >= MAX_TYPE_DEPTH {
        return None;
    }
    let callee = unparenthesize(callee);
    if callee.kind() == "func_literal" {
        return Some(callee);
    }
    if !matches!(callee.kind(), "identifier" | "type_identifier") {
        return None;
    }
    let name = local_name(source, callee)?;
    let owner = name.parent()?;
    if owner.kind() == "function_declaration" {
        return Some(owner);
    }
    if is_type_declaration(owner) || owner.kind() == "variadic_parameter_declaration" {
        return None;
    }
    let binding = declaration_binding(name, &source.text)?;
    if let Some(ty) = binding.explicit_type {
        return local_func_type(source, ty, depth + 1);
    }
    if binding.result_index == 0 {
        return local_callable(source, binding.initializer?, depth + 1);
    }
    None
}

fn local_func_type<'tree>(
    source: &'tree Source,
    mut ty: Node<'tree>,
    depth: usize,
) -> Option<Node<'tree>> {
    for _ in depth..MAX_TYPE_DEPTH {
        ty = unparenthesize(ty);
        if ty.kind() == "function_type" {
            return Some(ty);
        }
        if ty.kind() != "type_identifier" {
            return None;
        }
        let owner = local_name(source, ty)?.parent()?;
        if !is_type_declaration(owner) || owner.child_by_field_name("type_parameters").is_some() {
            return None;
        }
        ty = owner.child_by_field_name("type")?;
    }
    None
}

fn unshadowed_builtin<'a>(source: &'a Source, callee: Node<'_>) -> Option<(&'a str, &'static str)> {
    let callee = unparenthesize(callee);
    if callee.kind() != "identifier" {
        return None;
    }
    let name = node_text(callee, &source.text);
    let syntax = builtin_function(name)?;
    (local_name(source, callee).is_none()).then_some((name, syntax))
}

fn fallback_builtin(parser: &mut Parser, syntax: &str) -> Result<Option<Signature>> {
    let source = Source::parse(
        parser,
        PathBuf::new(),
        format!("package builtin\n{syntax}\n"),
        None,
    )?;
    let root = source.tree.root_node();
    Ok(root
        .named_children(&mut root.walk())
        .find(|node| node.kind() == "function_declaration")
        .and_then(|function| Signature::from_source(function, &source.text, &[])))
}

impl<'w> Resolver<'w, '_> {
    fn callable_type(
        &mut self,
        from: &'w Package,
        source: &Source,
        call: Call<'_>,
    ) -> Result<Option<TypeIdentity<'w>>> {
        let callee = unparenthesize(call.callee);
        let ty = self.infer_type(from, source, callee, 0, 0)?;
        let ty = match ty {
            Some(ty) => Some(ty),
            None if matches!(callee.kind(), "identifier" | "type_identifier") => {
                self.dot_imported_callable(from, source, callee)?
            }
            None => None,
        };
        let Some(mut ty) = ty.filter(|ty| !ty.type_expression) else {
            return Ok(None);
        };
        if let Some(list) = call.type_arguments {
            let arguments = explicit_type_arguments(list);
            let specialized = if matches!(&ty.kind, TypeKind::Function { signature, .. } if !signature.type_parameters.is_empty())
            {
                self.specialize_function(from, source, ty, arguments)?
            } else if arguments.len() == 1 {
                self.indexed_element(ty)?
            } else {
                None
            };
            let Some(specialized) = specialized else {
                return Ok(None);
            };
            ty = specialized;
        }
        Ok(self
            .underlying_type(ty)?
            .filter(|ty| ty.pointers == 0 && matches!(ty.kind, TypeKind::Function { .. })))
    }

    fn dot_imported_callable(
        &mut self,
        from: &'w Package,
        source: &Source,
        callee: Node<'_>,
    ) -> Result<Option<TypeIdentity<'w>>> {
        let name = node_text(callee, &source.text);
        if !is_exported(name) || lexical_binding(callee, name, &source.text).is_some() {
            return Ok(None);
        }
        let Some((path, _)) =
            self.indexes
                .find_dot_import(self.workspace, from, &source.imports, name)?
        else {
            return Ok(None);
        };
        let Some(definition) = self.indexes.reference_source(&path)? else {
            return Ok(None);
        };
        let package = self.declaring_package(definition.as_ref(), from);
        let Some(symbol) = self.find_symbol(package, name, None)? else {
            return Ok(None);
        };
        self.symbol_type(source, &symbol, name, 0)
    }

    fn present_callable(
        &mut self,
        from: &'w Package,
        source: &Source,
        call: Call<'_>,
        ty: TypeIdentity<'w>,
    ) -> Result<Option<Signature>> {
        let TypeIdentity {
            package,
            kind:
                TypeKind::Function {
                    file,
                    signature,
                    mut substitutions,
                    method_receiver,
                },
            ..
        } = ty
        else {
            return Ok(None);
        };
        if !signature.type_parameters.is_empty() {
            let arguments = call.arguments.expressions();
            let query = GenericCall {
                caller: from,
                source,
                arguments: &arguments,
                depth: 0,
                allow_partial: true,
            };
            let origin = TypeOrigin {
                package,
                file: &file,
            };
            if let Some(inferred) =
                self.infer_type_arguments(query, origin, &signature, &substitutions)?
            {
                substitutions = inferred;
            }
        }
        let mut types = Vec::new();
        for (name, ty) in substitutions {
            if let Some(ty) = ty
                && let Some(ty) = self.type_text(from, source, &ty)?
            {
                types.push((name, ty));
            }
        }
        let receiver = match method_receiver {
            Some(receiver) => self.type_text(from, source, &receiver)?,
            None => None,
        };
        let definition;
        let declaring = if path_key(&file) == path_key(&source.path) {
            source
        } else if let Some(disk) = self.indexes.reference_source(&file)? {
            definition = disk;
            definition.as_ref()
        } else {
            return Ok(None);
        };
        let signature = function_at(declaring, signature.origin.range())
            .and_then(|function| Signature::from_source(function, &declaring.text, &types));
        Ok(signature.map(|mut signature| {
            if let Some(receiver) = receiver {
                signature.parameters.insert(0, receiver);
            }
            signature
        }))
    }

    fn builtin_signature(
        &mut self,
        root: &Path,
        name: &str,
        fallback: &str,
    ) -> Result<Option<Signature>> {
        if let Ok(file) = self.workspace.builtin_source(root)
            && let Ok(Some(source)) = self.indexes.reference_source(file)
        {
            let source = source.as_ref();
            let symbols = &source.declarations().symbols;
            let symbol = if name == "Error" {
                symbols.member("error", name)
            } else {
                symbols.declaration(name)
            };
            if let Some(symbol) = symbol
                && let Some(function) = function_at(source, symbol.range)
                && let Some(signature) = Signature::from_source(function, &source.text, &[])
            {
                return Ok(Some(signature));
            }
        }
        fallback_builtin(self.indexes.parser, fallback)
    }

    pub(super) fn unify_function_types(
        &mut self,
        expected: TypeIdentity<'w>,
        actual: TypeIdentity<'w>,
        bindings: &mut [Option<TypeIdentity<'w>>],
    ) -> Result<bool> {
        let (Some(expected), Some(actual)) = (
            self.callable_components(expected)?,
            self.callable_components(actual)?,
        ) else {
            return Ok(false);
        };
        if expected.variadic != actual.variadic
            || expected.parameters.len() != actual.parameters.len()
            || expected.results.len() != actual.results.len()
        {
            return Ok(false);
        }
        for (left, right) in expected
            .parameters
            .into_iter()
            .chain(expected.results)
            .zip(actual.parameters.into_iter().chain(actual.results))
        {
            if !self.unify_types(left, right, bindings, false)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn callable_components(
        &mut self,
        ty: TypeIdentity<'w>,
    ) -> Result<Option<CallableComponents<'w>>> {
        let TypeKind::Function {
            file,
            signature,
            substitutions,
            method_receiver,
        } = ty.kind
        else {
            return Ok(None);
        };
        let parsed;
        let parameters = if signature.parameters.is_empty()
            && signature.origin.function_type().is_none()
            && let Some(source) = self.indexes.reference_source(&file)?
            && let Some(function) = function_at(source.as_ref(), signature.origin.range())
            && let Some(list) = function.child_by_field_name("parameters")
        {
            let source = source.as_ref();
            parsed = symbols::parameter_types(list, |node| {
                symbols::type_ref(node, &source.text, &|node| source.range(node))
            });
            &parsed
        } else {
            &signature.parameters
        };
        let imports = &self.indexes.file_index(&file)?.imports;
        let materialize = |reference: &Option<TypeRef>| {
            materialize_type(
                self.workspace,
                ty.package,
                &file,
                imports,
                reference.clone()?,
                &substitutions,
            )
        };
        let parameters = method_receiver
            .into_iter()
            .map(|receiver| Some(*receiver))
            .chain(parameters.iter().map(materialize))
            .collect::<Option<Vec<_>>>();
        let results = signature
            .results
            .iter()
            .map(materialize)
            .collect::<Option<Vec<_>>>();
        Ok(parameters
            .zip(results)
            .map(|(parameters, results)| CallableComponents {
                parameters,
                results,
                variadic: signature.variadic,
            }))
    }
}

struct CallableComponents<'w> {
    parameters: Vec<TypeIdentity<'w>>,
    results: Vec<TypeIdentity<'w>>,
    variadic: bool,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/fixtures/hover-signature")
    }

    fn open(text: &str) -> (Engine, PathBuf) {
        let root = fixture_root();
        let path = root.join("main.go");
        let mut engine = Engine::new(root);
        engine.open(path.clone(), text.into()).unwrap();
        (engine, path)
    }

    fn marked(text: &str) -> (String, Position) {
        let offset = text.find('|').expect("caret marker");
        let text = text.replacen('|', "", 1);
        let position = LineIndex::new(&text).position(&text, offset);
        (text, position)
    }

    fn lookup(engine: &mut Engine, path: &Path, position: Position) -> Option<SignatureHelp> {
        match engine.signature_help(path, position).unwrap() {
            QueryProgress::Ready(help) => return help,
            QueryProgress::Pending(load) => {
                let workspace = Workspace::load(engine.root(), load.include_tests).unwrap();
                assert!(engine.install_workspace(load, workspace));
            }
            QueryProgress::Searching(_) => panic!("signature help started a search"),
        }
        match engine.signature_help(path, position).unwrap() {
            QueryProgress::Ready(help) => help,
            _ => panic!("signature help did not finish after loading the workspace"),
        }
    }

    fn labels(help: &SignatureHelp) -> Vec<String> {
        assert_eq!(help.active_signature, Some(0));
        let [signature] = &help.signatures[..] else {
            panic!("expected one signature")
        };
        let units = signature.label.encode_utf16().collect::<Vec<_>>();
        signature
            .parameters
            .as_ref()
            .unwrap()
            .iter()
            .map(|parameter| {
                let ParameterLabel::LabelOffsets([start, end]) = parameter.label else {
                    panic!("expected UTF-16 offsets");
                };
                String::from_utf16(&units[start as usize..end as usize]).unwrap()
            })
            .collect()
    }

    fn assert_call(help: &SignatureHelp, parameters: &[&str], active: Option<u32>) {
        assert_eq!(labels(help), parameters, "{}", help.signatures[0].label);
        assert_eq!(help.active_parameter, active);
    }

    fn docs(help: &SignatureHelp) -> Option<&str> {
        match &help.signatures[0].documentation {
            Some(Documentation::MarkupContent(doc)) => {
                assert_eq!(doc.kind, MarkupKind::Markdown);
                Some(&doc.value)
            }
            _ => None,
        }
    }

    #[test]
    fn expands_names_and_counts_utf16_units_in_label_offsets() {
        let (text, position) = marked(concat!(
            "package main\n\n// 𐐀 accepts a label and counts.\n//go:noinline\n",
            "func 𐐀(𐐁 string, 字, 次 int) (总, 数 int, err error) { return 0, 0, nil }\n",
            "func run() { 𐐀(\"😀\", 2, |3) }\n",
        ));
        let (mut engine, path) = open(&text);
        let help = lookup(&mut engine, &path, position).unwrap();
        assert_call(&help, &["𐐁 string", "字 int", "次 int"], Some(2));
        let signature = &help.signatures[0];
        assert_eq!(
            signature.label,
            "func 𐐀(𐐁 string, 字 int, 次 int) (总 int, 数 int, err error)"
        );
        assert_eq!(
            signature.parameters.as_ref().unwrap()[0].label,
            ParameterLabel::LabelOffsets([8, 17]),
        );
        assert_eq!(docs(&help), Some("𐐀 accepts a label and counts."));
        assert!(engine.workspace.is_none());
    }

    #[test]
    fn counts_top_level_commas_and_clamps_variadic_parameter() {
        let prefix = r#"package main
func head(s string, items ...string) {}
func inner(x, y int) string { return "" }
func run() {
"#;
        for (expr, expected, active) in [
            (
                r#"head(inner(1, 2), "x,(", `a,b)`, /* , ) */ |"z")"#,
                vec!["s string", "items ...string"],
                1,
            ),
            ("head(inner(1, |2), \"a\")", vec!["x int", "y int"], 1),
            (
                "head(|inner(1, 2), \"a\")",
                vec!["s string", "items ...string"],
                0,
            ),
            (
                "head(inner(1, 2)|, \"a\")",
                vec!["s string", "items ...string"],
                0,
            ),
            (
                "head(inner(1, 2),| \"a\")",
                vec!["s string", "items ...string"],
                1,
            ),
        ] {
            let (text, position) = marked(&format!("{prefix}{expr}\n}}\n"));
            let (mut engine, path) = open(&text);
            let help = lookup(&mut engine, &path, position).unwrap_or_else(|| panic!("{expr}"));
            assert_call(&help, &expected, Some(active));
        }
    }

    #[test]
    fn stops_at_literal_bracket_and_call_boundaries() {
        let prefix = "package main\nfunc f(arg any) {}\nfunc run(items []int) { ";
        for expr in [
            "f|(1)",
            "f(1)|",
            "|f(1)",
            "f(|func(x int) { _ = x })",
            "f(func(x |int) { _ = x })",
            "f(func(x int) { _ = |x })",
            "f(struct{ A int }{A: |2})",
            "f(items[|1])",
            "f(items[0:|1])",
            "f(func(x int) { f(|\n",
        ] {
            let (text, position) = marked(&format!("{prefix}{expr} \n}}\n"));
            let (mut engine, path) = open(&text);
            if expr.contains(" { f(") {
                // An independent inner call can be found within the closure.
                assert_call(
                    &lookup(&mut engine, &path, position).unwrap(),
                    &["arg any"],
                    Some(0),
                );
            } else {
                assert!(lookup(&mut engine, &path, position).is_none(), "{expr}");
            }
        }
        let (text, position) = marked(&format!("{prefix}f(1|) }}\n"));
        let (mut engine, path) = open(&text);
        assert_call(
            &lookup(&mut engine, &path, position).unwrap(),
            &["arg any"],
            Some(0),
        );
    }

    #[test]
    fn repairs_unclosed_calls_and_prefers_the_inner_call() {
        let header =
            "package main\nfunc foo(a int, b string) bool { return false }\nfunc outer(x any) {}\n";
        for (tail, active) in [
            ("func run() { foo(|", 0),
            ("func run() { foo(1, |", 1),
            ("func run() { foo(1, |\n}\n", 1),
            ("func run() { outer(foo(1, |\n}\n", 1),
            ("func run() { foo(1, /* ) , */ |\n}\n", 1),
        ] {
            let (text, position) = marked(&format!("{header}{tail}"));
            let (mut engine, path) = open(&text);
            let help = lookup(&mut engine, &path, position).unwrap_or_else(|| {
                panic!(
                    "missing signature in {tail}: {}",
                    engine.documents[&path_key(&path)]
                        .tree
                        .root_node()
                        .to_sexp()
                )
            });
            assert_call(&help, &["a int", "b string"], Some(active));
            assert!(engine.workspace.is_none(), "{tail}");
        }
    }

    #[test]
    fn resolves_a_method_in_an_unclosed_call_with_a_following_block_close() {
        let (text, position) = marked(concat!(
            "package main\n",
            "type Counter struct{}\n",
            "func (Counter) Set(value int, label string) {}\n",
            "func run(c Counter) { c.Set(1, |\n}\n",
        ));
        let (mut engine, path) = open(&text);
        let help = lookup(&mut engine, &path, position).unwrap_or_else(|| {
            panic!(
                "missing method in {}",
                engine.documents[&path_key(&path)]
                    .tree
                    .root_node()
                    .to_sexp()
            )
        });
        assert_call(&help, &["value int", "label string"], Some(1));
    }

    #[test]
    fn specializes_generic_functions_receiver_methods_and_method_expressions() {
        let source = r#"package main
import "example.com/hoversig/helper"
type Embedded struct { helper.Box[int] }
func render(x int) string { return "" }
func run(box helper.IntBox, value Embedded) {
    _ = helper.Map([]int{1}, func(x int) string { return "" })
    _ = helper.Map([]int{1}, render)
    _ = helper.Map[int, string]([]int{1}, render)
    box.Set(1)
    value.Set(2)
    (*helper.Box[int]).Set(&box, 3)
    configure := (*helper.Box[int]).Set
    configure(&box, 4)
}
"#;
        let (mut engine, path) = open(source);
        for (anchor, len, parameters, active) in [
            (
                "helper.Map([]int{1}, func",
                "helper.Map(".len(),
                vec!["items []int", "fn func(int) string"],
                0,
            ),
            (
                "helper.Map([]int{1}, render",
                "helper.Map(".len(),
                vec!["items []int", "fn func(int) string"],
                0,
            ),
            (
                "helper.Map[int, string]([]",
                "helper.Map[int, string](".len(),
                vec!["items []int", "fn func(int) string"],
                0,
            ),
            ("box.Set(1)", "box.Set(".len(), vec!["v int"], 0),
            ("value.Set(2)", "value.Set(".len(), vec!["v int"], 0),
            (
                "Set(&box, 3)",
                "Set(&box, ".len(),
                vec!["*helper.Box[int]", "v int"],
                1,
            ),
            (
                "configure(&box, 4)",
                "configure(".len(),
                vec!["*helper.Box[int]", "v int"],
                0,
            ),
        ] {
            let offset = source.find(anchor).unwrap() + len;
            let position = LineIndex::new(source).position(source, offset);
            let help = lookup(&mut engine, &path, position)
                .unwrap_or_else(|| panic!("missing signature at {anchor}"));
            assert_call(&help, &parameters, Some(active));
            let documentation = docs(&help).unwrap();
            assert!(documentation.contains(if anchor.contains("Map") {
                "Map transforms"
            } else {
                "Set replaces"
            }));
            assert!(!documentation.contains("go:noinline"));
        }
    }

    #[test]
    fn infers_partial_and_variadic_generic_arguments() {
        let header = "package main\nfunc Pair[A, B any](first A, second B, more ...B) {}\n";
        for (tail, expected, active) in [
            ("Pair(-1, |", vec!["first int", "second B", "more ...B"], 1),
            (
                "Pair(1, \"b\", \"c\", |\"d\")",
                vec!["first int", "second string", "more ...string"],
                2,
            ),
            (
                "Pair(1, \"b\", |[]string{}...)",
                vec!["first int", "second string", "more ...string"],
                2,
            ),
            (
                "Pair[int](1, \"b\", |\"c\")",
                vec!["first int", "second string", "more ...string"],
                2,
            ),
        ] {
            let (text, position) = marked(&format!("{header}func run() {{ {tail}\n}}\n"));
            let (mut engine, path) = open(&text);
            let help = lookup(&mut engine, &path, position).unwrap_or_else(|| panic!("{tail}"));
            assert_call(&help, &expected, Some(active));
        }
    }

    #[test]
    fn resolves_func_values_named_types_fields_results_and_closures() {
        let program = r#"package main
// Op applies a labeled operation.
type Op func(count int, label string) bool
type Alias = Op
type Holder struct { callback Alias }
func factory() func(name string) { return nil }
func run(callback Alias, holder Holder, table []Alias) {
"#;
        for (expression, expected, active) in [
            ("callback(1, |\"x\")", vec!["count int", "label string"], 1),
            (
                "holder.callback(|1, \"x\")",
                vec!["count int", "label string"],
                0,
            ),
            ("factory()(|\"name\")", vec!["name string"], 0),
            ("func(amount int) {}(|1)", vec!["amount int"], 0),
            ("table[0](|1, \"x\")", vec!["count int", "label string"], 0),
        ] {
            let (text, position) = marked(&format!("{program}{expression}\n}}\n"));
            let (mut engine, path) = open(&text);
            let help =
                lookup(&mut engine, &path, position).unwrap_or_else(|| panic!("{expression}"));
            assert_call(&help, &expected, Some(active));
        }
        let (text, position) = marked(&format!("{program}_ = Op(|callback)\n}}\n"));
        let (mut engine, path) = open(&text);
        assert!(lookup(&mut engine, &path, position).is_none());
    }

    #[test]
    fn uses_sdk_builtin_docs_and_covers_fallback_and_shadowing() {
        let (text, position) =
            marked("package main\nfunc run(values []int) { append(values, 1, |2) }\n");
        let (mut engine, path) = open(&text);
        let fallback = engine
            .signature_help_after_load_failure(&path, position)
            .unwrap()
            .unwrap();
        assert_call(&fallback, &["slice []Type", "elems ...Type"], Some(1));
        assert!(docs(&fallback).is_none());
        let sdk = lookup(&mut engine, &path, position).unwrap();
        assert_call(&sdk, &["slice []Type", "elems ...Type"], Some(1));
        assert!(
            docs(&sdk)
                .unwrap()
                .contains("appends elements to the end of a slice")
        );
        for name in [
            "append", "make", "new", "len", "cap", "copy", "delete", "close", "panic", "recover",
            "min", "max", "clear", "complex", "real", "imag", "print", "println",
        ] {
            assert!(
                fallback_builtin(&mut engine.parser, builtin_function(name).unwrap())
                    .unwrap()
                    .is_some(),
                "{name}"
            );
        }
        let (text, position) = marked("package main\nfunc run(len func(v bool)) { len(|true) }\n");
        let (mut engine, path) = open(&text);
        assert_call(
            &lookup(&mut engine, &path, position).unwrap(),
            &["v bool"],
            Some(0),
        );
        let (text, position) =
            marked("package main\nfunc run(err error) { _ = error.Error(|err) }\n");
        let (mut engine, path) = open(&text);
        assert_call(
            &lookup(&mut engine, &path, position).unwrap(),
            &["error"],
            Some(0),
        );
    }

    #[test]
    fn unsaved_dependency_edits_and_close_update_signature_and_docs() {
        let root = fixture_root();
        let caller = root.join("main.go");
        let helper = root.join("helper/helper.go");
        let callers = fs::read_to_string(&caller).unwrap();
        let target = fs::read_to_string(&helper).unwrap();
        let offset = callers.find("helper.Join(").unwrap() + "helper.Join(".len();
        let position = LineIndex::new(&callers).position(&callers, offset);
        let mut engine = Engine::new(root);
        engine.open(caller.clone(), callers).unwrap();
        engine.open(helper.clone(), target.clone()).unwrap();
        let original = lookup(&mut engine, &caller, position).unwrap();
        assert_call(&original, &["sep string", "parts ...string"], Some(0));
        engine
            .edit(
                &helper,
                &[TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: target.replace("sep string", "separator string").replace(
                        "Join concatenates parts separated by sep.",
                        "Join has edited docs.",
                    ),
                }],
            )
            .unwrap();
        let edited = lookup(&mut engine, &caller, position).unwrap();
        assert_call(&edited, &["separator string", "parts ...string"], Some(0));
        assert_eq!(docs(&edited), Some("Join has edited docs."));
        engine.close(&helper);
        assert_eq!(lookup(&mut engine, &caller, position), Some(original));
    }
}
