use super::super::Engine;
use super::super::resolver::Resolver;
use super::super::source::{ReferenceQuery, Source, is_declaration_name, node_text};
use crate::workspace::{Package, Workspace, is_symbol_source, path_key};
use anyhow::Result;
use lsp_types::{Position, Range};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(super) struct Target {
    pub location: (PathBuf, Range),
    pub spelling: String,
    pub range: Range,
    pub implicit_import: bool,
    pub file_local: bool,
}

pub(super) fn binding<'w>(
    resolver: &mut Resolver<'w, '_>,
    package: &'w Package,
    source: &Source,
    reference: ReferenceQuery<'_>,
    root: &Path,
) -> Result<Option<(PathBuf, Range)>> {
    let node = reference.name();
    if !matches!(
        node.kind(),
        "identifier"
            | "field_identifier"
            | "type_identifier"
            | "package_identifier"
            | "true"
            | "false"
            | "iota"
            | "nil"
    ) || node_text(node, &source.text) == "_"
    {
        return Ok(None);
    }
    if let Some(parent) = node.parent() {
        match parent.kind() {
            "package_clause" => return Ok(None),
            "import_spec" => {
                return Ok((parent.child_by_field_name("name") == Some(node))
                    .then(|| (source.path.clone(), source.range(node))));
            }
            _ => {}
        }
    }
    if matches!(reference, ReferenceQuery::Name(_)) && is_declaration_name(node) {
        return Ok(Some((source.path.clone(), source.range(node))));
    }
    resolver.reference_location(package, source, reference, root)
}

pub(super) fn target(
    engine: &mut Engine,
    path: &Path,
    position: Position,
) -> Result<Option<Target>> {
    let mut snapshot = engine.snapshot();
    let Some(document) = snapshot.document(path)? else {
        return Ok(None);
    };
    let source = document.indexed();
    let Some(reference) = source.reference_at(position) else {
        return Ok(None);
    };
    let node = reference.name();
    let range = source.range(node);
    if position < range.start || position >= range.end {
        return Ok(None);
    }
    let spelling = node_text(node, &source.text);
    let Some(workspace) = snapshot.workspace() else {
        return Ok(None);
    };
    let Some(package) = snapshot.active_package(workspace, source) else {
        return Ok(None);
    };
    let root = snapshot.root();
    let mut resolver = snapshot.resolver(workspace);
    let Some(location) = binding(&mut resolver, package, source, reference, root)? else {
        return Ok(None);
    };
    if !writable(
        workspace,
        &location.0,
        snapshot.docs().contains_key(&path_key(&location.0)),
    ) {
        return Ok(None);
    }
    let Some(origin) = snapshot.document(&location.0)? else {
        return Ok(None);
    };
    let declaration = origin.indexed();
    if snapshot.active_package(workspace, declaration).is_none() {
        return Ok(None);
    }
    let Some(anchor) = declaration
        .reference_at(location.1.start)
        .map(|r| r.name())
        .filter(|node| declaration.range(*node) == location.1)
    else {
        return Ok(None);
    };
    let implicit_import = declaration
        .imports
        .iter()
        .any(|import| import.alias.is_none() && import.binding == location.1);
    if !implicit_import && node_text(anchor, &declaration.text) != spelling {
        return Ok(None);
    }
    if let Some(parent) = anchor.parent()
        && parent.kind() == "function_declaration"
        && parent.child_by_field_name("name") == Some(anchor)
        && (spelling == "init"
            || (spelling == "main" && declaration.outline().package.as_deref() == Some("main")))
    {
        return Ok(None);
    }
    // An embedded field name is also a use of its type. A selection can
    // resolve to this token instead of the named type's declaration.
    if super::scan::embedded_type_use(anchor) {
        let reference = declaration.reference_at(location.1.start).unwrap();
        let package = snapshot.active_package(workspace, declaration).unwrap();
        let mut resolver = snapshot.resolver(workspace);
        if binding(&mut resolver, package, declaration, reference, root)?
            .is_none_or(|(p, r)| path_key(&p) == path_key(&location.0) && r == location.1)
        {
            return Ok(None);
        }
        return Ok(
            target(engine, &location.0, location.1.start)?.map(|mut target| {
                target.range = range;
                target
            }),
        );
    }
    let file_local = anchor.parent().is_some_and(|parent| {
        if parent.kind() == "import_spec" {
            return true;
        }
        if matches!(
            parent.kind(),
            "method_elem" | "method_declaration" | "field_declaration" | "function_declaration"
        ) && is_declaration_name(anchor)
        {
            return false;
        }
        std::iter::successors(Some(parent), |n| n.parent()).any(|n| {
            matches!(
                n.kind(),
                "parameter_declaration"
                    | "variadic_parameter_declaration"
                    | "type_parameter_declaration"
                    | "function_declaration"
                    | "method_declaration"
                    | "func_literal"
            )
        })
    });
    let target = Target {
        spelling: spelling.to_owned(),
        location,
        range,
        implicit_import,
        file_local,
    };
    // Navigation may return the first declaration in a malformed package.
    // Preparing an edit requires a single declaration in the relevant scope.
    if super::collisions::check_collisions(
        engine,
        std::slice::from_ref(&target.location),
        &target.spelling,
    )
    .is_err()
    {
        return Ok(None);
    }
    Ok(Some(target))
}

pub(super) fn writable(workspace: &Workspace, path: &Path, open: bool) -> bool {
    let eligible = |path: &Path| {
        let key = path_key(path);
        is_symbol_source(&key)
            && workspace.contains_project_path(&key)
            && !workspace.contains_external_symbol_path(&key)
    };
    if !eligible(path) {
        return false;
    }
    match path.metadata() {
        Ok(meta) => {
            meta.is_file()
                && !meta.permissions().readonly()
                && path
                    .canonicalize()
                    .is_ok_and(|resolved| eligible(&resolved))
        }
        Err(error) if open && error.kind() == std::io::ErrorKind::NotFound => path
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .zip(path.file_name())
            .is_some_and(|(parent, name)| eligible(&parent.join(name))),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestProject;
    use crate::workspace::Workspace;
    use serde_json::json;

    #[test]
    fn resolves_import_alias_at_its_declaration_and_use_and_excludes_the_path() {
        let project = TestProject::new();
        let imported = project.root.join("model");
        project.write("model/types.go", "package dep\ntype T struct{}\n");
        for (import, use_name) in [("alias \"test/model\"", "alias"), ("\"test/model\"", "dep")] {
            let text = format!("package sample\nimport {import}\nvar _ {use_name}.T\n");
            let path = project.write("sample.go", &text);
            let packages = format!(
                "{}\n{}",
                json!({"Dir":project.root,"ImportPath":"test/api","Name":"sample","GoFiles":["sample.go"],"Imports":["test/model"]}),
                json!({"Dir":imported,"ImportPath":"test/model","Name":"dep","GoFiles":["types.go"]})
            );
            let mut engine = Engine::new(project.root.clone());
            engine.install_workspace(
                engine.workspace_load(),
                Workspace::from_test_listing(std::slice::from_ref(&project.root), &packages, true),
            );
            let chosen = target(&mut engine, &path, Position::new(2, 6))
                .unwrap()
                .unwrap();
            assert_eq!(chosen.spelling, use_name);
            assert_eq!(chosen.range.start, Position::new(2, 6));
            assert_eq!(
                chosen.location,
                (
                    path.clone(),
                    Range::new(
                        Position::new(1, 7),
                        Position::new(1, if use_name == "alias" { 12 } else { 19 })
                    )
                )
            );
            assert_eq!(chosen.implicit_import, use_name == "dep");
            assert!(chosen.file_local);
            let literal = text.lines().nth(1).unwrap().find('"').unwrap() as u32;
            assert!(
                target(&mut engine, &path, Position::new(1, literal + 1))
                    .unwrap()
                    .is_none()
            );
            if use_name == "alias" {
                assert_eq!(
                    target(&mut engine, &path, Position::new(1, 7))
                        .unwrap()
                        .unwrap()
                        .location,
                    chosen.location
                );
            }
        }
    }

    #[test]
    fn distinguishes_function_bindings_from_fields_in_a_local_type() {
        let project = TestProject::new();
        let code = "package sample\ntype Outer[T any] struct{ X T }\nfunc f(p int){ type L struct{ X, Y int }; q := p; _=q }\n";
        let path = project.write("sample.go", code);
        let mut engine = Engine::new(project.root.clone());
        engine.install_workspace(
            engine.workspace_load(),
            project.workspace(&["sample.go"], true),
        );
        for (fragment, local) in [
            ("Outer[", false),
            ("T any", true),
            ("X T", false),
            ("p int", true),
            ("L struct", true),
            ("Y int", false),
            ("q :=", true),
        ] {
            let pos = engine
                .snapshot()
                .document(&path)
                .unwrap()
                .unwrap()
                .indexed()
                .lines
                .position(code, code.find(fragment).unwrap());
            assert_eq!(
                target(&mut engine, &path, pos).unwrap().unwrap().file_local,
                local,
                "{fragment}"
            );
        }
    }

    #[test]
    fn refuses_ambiguous_package_symbol_and_comment_prefixed_main() {
        let project = TestProject::new();
        let a = project.write(
            "a.go",
            "// license\npackage main\nvar Clash int\nfunc main() {}\nfunc f(){ _=Clash }\n",
        );
        project.write("b.go", "package main\nvar Clash int\n");
        let mut engine = Engine::new(project.root.clone());
        engine.install_workspace(
            engine.workspace_load(),
            project.workspace(&["a.go", "b.go"], true),
        );
        for position in [Position::new(4, 12), Position::new(3, 5)] {
            assert!(
                target(&mut engine, &a, position).unwrap().is_none(),
                "{position:?}"
            );
        }
    }

    #[test]
    fn accepts_shadowed_builtins_and_excludes_special_functions_comments_and_whitespace() {
        let project = TestProject::new();
        let text = "package main\nfunc main() {}\nfunc init() {}\nfunc f(len int) int { return len } // len\n";
        let path = project.write("sample.go", text);
        let mut engine = Engine::new(project.root.clone());
        engine.install_workspace(
            engine.workspace_load(),
            project.workspace(&["sample.go"], true),
        );
        assert_eq!(
            target(&mut engine, &path, Position::new(3, 29))
                .unwrap()
                .unwrap()
                .location
                .1
                .start,
            Position::new(3, 7)
        );
        for (line, col) in [(0, 9), (1, 5), (2, 5), (3, 10), (3, 38)] {
            assert!(
                target(&mut engine, &path, Position::new(line, col))
                    .unwrap()
                    .is_none(),
                "{line}:{col}"
            );
        }
    }
}
