use crate::semantic::Engine;
use crate::semantic::implementation::RenameMethod;
use crate::semantic::index::text_fingerprint;
use crate::semantic::resolver::{Resolver, TypeIdentity};
use crate::semantic::source::is_type_declaration;
use crate::symbols::{SymbolKind, TypeRefKind};
use crate::workspace::{Package, path_key};
use anyhow::{Context, Result, ensure};
use lsp_types::Range;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;

type Location = (PathBuf, Range);

#[path = "destination.rs"]
mod destination;
pub(super) use destination::{MethodDestinations, method_destinations};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct MethodRelation {
    interface_package: String,
    interface: String,
    implementer_package: String,
    implementer: String,
    pointer: bool,
}

#[derive(Debug)]
pub(super) struct MethodContracts {
    pub(super) family: Vec<Location>,
    pub(super) relations: BTreeSet<MethodRelation>,
    pub(super) is_method: bool,
    pub(super) disk_inputs: HashMap<PathBuf, u64>,
}

struct Receiver<'w> {
    name: String,
    ty: TypeIdentity<'w>,
    method: RenameMethod,
}

pub(super) struct FieldDestination {
    pub(super) receivers: BTreeSet<(String, String, bool)>,
    pub(super) disk_inputs: HashMap<PathBuf, u64>,
}

pub(super) fn field_destination(
    engine: &mut Engine,
    target: &Location,
    name: &str,
) -> Result<Option<FieldDestination>> {
    let mut snapshot = engine.snapshot();
    let document = snapshot
        .document(&target.0)?
        .context("missing field declaration document")?;
    let node = document
        .indexed()
        .reference_at(target.1.start)
        .map(|r| r.name());
    if !node.is_some_and(|n| {
        super::scan::embedded_type_use(n)
            || n.parent().is_some_and(|p| {
                p.kind() == "field_declaration" && super::super::source::is_declaration_name(n)
            })
    }) {
        return Ok(None);
    }
    let workspace = snapshot
        .workspace()
        .context("field method sets require the active package graph")?;
    let owner = snapshot
        .active_package(workspace, document.indexed())
        .context("missing field package")?;
    let mut resolver = snapshot.resolver(workspace);
    let mut disk_inputs = HashMap::new();
    let candidates = method_packages(&mut resolver, name, &mut disk_inputs)?;
    // Only this package and its transitive importers can embed a type
    // containing this field. Open files can add further workspace imports.
    let mut affected = HashSet::new();
    let mut pending: Vec<_> = workspace.same_package_variants(owner).collect();
    while let Some(package) = pending.pop() {
        if affected.insert(package.id.as_str()) {
            pending.extend(workspace.direct_importers(package));
        }
    }
    let mut receivers = BTreeSet::new();
    for id in candidates {
        let package = workspace
            .package(&id)
            .context("missing field method candidate")?;
        if !affected.contains(id.as_str()) && !workspace.is_project_package(package) {
            continue;
        }
        ensure!(
            !package.load_error,
            "cannot verify field promotion in {}",
            package.id
        );
        let mut found = Vec::new();
        collect_receivers(&mut resolver, package, owner, name, &mut found)?;
        for receiver in found {
            if resolver.rename_value_has_method(&receiver.ty, &receiver.method)? {
                receivers.insert((id.clone(), receiver.name.clone(), false));
            }
            if !receiver.method.is_interface() {
                receivers.insert((id.clone(), receiver.name, true));
            }
        }
    }
    Ok(Some(FieldDestination {
        receivers,
        disk_inputs,
    }))
}

pub(super) fn method_contracts(
    engine: &mut Engine,
    target: &Location,
    name: &str,
) -> Result<MethodContracts> {
    let mut snapshot = engine.snapshot();
    let document = snapshot
        .document(&target.0)?
        .context("missing declaration document")?;
    let node = document
        .indexed()
        .reference_at(target.1.start)
        .map(|r| r.name());
    if !node.and_then(|n| n.parent()).is_some_and(|n| {
        matches!(n.kind(), "method_elem" | "method_declaration")
            && n.child_by_field_name("name") == node
    }) {
        return Ok(MethodContracts {
            family: vec![target.clone()],
            relations: BTreeSet::new(),
            is_method: false,
            disk_inputs: HashMap::new(),
        });
    }
    let workspace = snapshot
        .workspace()
        .context("method contracts require the active package graph")?;
    let owner = snapshot
        .active_package(workspace, document.indexed())
        .context("method owner is outside the active package graph")?;
    let mut resolver = snapshot.resolver(workspace);
    let mut receivers = Vec::new();
    let mut disk_inputs = HashMap::new();
    let packages = method_packages(&mut resolver, name, &mut disk_inputs)?;
    for id in packages {
        let package = workspace
            .package(&id)
            .context("missing method candidate package")?;
        ensure!(
            !package.load_error,
            "could not load package {} to verify method {name}",
            package.id
        );
        collect_receivers(&mut resolver, package, owner, name, &mut receivers)?;
    }
    let key = |location: &Location| (path_key(&location.0), location.1.start, location.1.end);
    let mut locations = BTreeMap::new();
    for receiver in &receivers {
        locations.insert(
            key(&receiver.method.location),
            receiver.method.location.clone(),
        );
    }
    let target_key = key(target);
    ensure!(
        locations.contains_key(&target_key),
        "could not prove the method set for {name}"
    );
    let mut edges = HashMap::<_, Vec<(_, MethodRelation)>>::new();
    for interface in receivers.iter().filter(|r| r.method.is_interface()) {
        for implementer in &receivers {
            for pointer in [false, true] {
                if (pointer && implementer.method.is_interface())
                    || !resolver.rename_satisfies(&interface.method, &implementer.ty, pointer)?
                {
                    continue;
                }
                let left = key(&interface.method.location);
                let right = key(&implementer.method.location);
                let relation = MethodRelation {
                    interface_package: interface.ty.package.id.clone(),
                    interface: interface.name.clone(),
                    implementer_package: implementer.ty.package.id.clone(),
                    implementer: implementer.name.clone(),
                    pointer,
                };
                edges
                    .entry(left.clone())
                    .or_default()
                    .push((right.clone(), relation.clone()));
                edges.entry(right).or_default().push((left, relation));
            }
        }
    }
    let mut relations = BTreeSet::new();
    let mut pending = VecDeque::from([target_key]);
    let mut family_keys = BTreeSet::new();
    while let Some(location) = pending.pop_front() {
        if !family_keys.insert(location.clone()) {
            continue;
        }
        ensure!(
            workspace.contains_project_path(&location.0)
                && !workspace.contains_external_symbol_path(&location.0),
            "method {name} has a read-only contract in {}",
            location.0.display()
        );
        for (neighbor, relation) in edges.remove(&location).unwrap_or_default() {
            relations.insert(relation);
            pending.push_back(neighbor);
        }
    }
    ensure!(
        !receivers.iter().any(
            |r| r.method.is_predeclared_error && family_keys.contains(&key(&r.method.location))
        ),
        "method {name} implements the read-only predeclared error interface"
    );
    let family = family_keys
        .into_iter()
        .map(|k| {
            locations
                .remove(&k)
                .expect("a method graph node has a declaration")
        })
        .collect();
    Ok(MethodContracts {
        family,
        relations,
        is_method: true,
        disk_inputs,
    })
}

// Every method originates in a declaration. Start with the packages that
// declare this exact name, then include the packages that may promote it.
// All workspace packages participate because open files can add import edges.
fn method_packages(
    resolver: &mut Resolver<'_, '_>,
    name: &str,
    inputs: &mut HashMap<PathBuf, u64>,
) -> Result<HashSet<String>> {
    let workspace = resolver.workspace;
    let mut examined = HashMap::new();
    let mut pending = Vec::new();
    for package in workspace.navigation_packages() {
        let mut eligible = workspace.is_project_package(package) || package.load_error;
        for path in resolver.indexes.files(package).into_owned() {
            let key = path_key(&path);
            if let Some(&declares) = examined.get(&key) {
                eligible |= declares;
                continue;
            }
            let could_declare = if let Some(open) = resolver.indexes.documents.get(&key) {
                could_declare_method(&open.text, name)
            } else {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                let hash = text_fingerprint(&text);
                inputs.insert(key.clone(), hash);
                // Discard stale lazy indexes before using method signatures.
                if resolver
                    .indexes
                    .disk_index
                    .get(&key)
                    .is_some_and(|idx| idx.fingerprint != hash)
                {
                    resolver.indexes.disk_index.remove(&key);
                }
                could_declare_method(&text, name)
            };
            let declares = if !could_declare {
                false
            } else {
                let source = resolver
                    .indexes
                    .reference_source(&path)?
                    .context("method candidate disappeared")?;
                let source = source.as_ref();
                source.name_offsets(name).iter().any(|&offset| {
                    let Some(node) = source.reference_at_offset(offset).map(|r| r.name()) else {
                        return false;
                    };
                    node.parent().is_some_and(|p| {
                        matches!(p.kind(), "method_elem" | "method_declaration")
                            && p.child_by_field_name("name") == Some(node)
                    })
                })
            };
            examined.insert(key, declares);
            eligible |= declares;
        }
        if eligible {
            pending.push(package);
        }
    }
    let mut result = HashSet::new();
    while let Some(package) = pending.pop() {
        if result.insert(package.id.clone()) {
            pending.extend(workspace.direct_importers(package));
        }
    }
    Ok(result)
}

fn could_declare_method(text: &str, name: &str) -> bool {
    text.match_indices(name).any(|(start, _)| {
        if text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return false;
        }
        let rest = text[start + name.len()..].trim_start();
        rest.starts_with('(') || rest.starts_with('/')
    })
}

fn collect_receivers<'w>(
    resolver: &mut Resolver<'w, '_>,
    package: &'w Package,
    declaring_package: &Package,
    name: &str,
    receivers: &mut Vec<Receiver<'w>>,
) -> Result<()> {
    let mut names = HashSet::new();
    for file in resolver.indexes.files(package).into_owned() {
        let Some(source) = resolver.indexes.reference_source(&file)? else {
            anyhow::bail!("missing method candidate file {}", file.display());
        };
        let source = source.as_ref();
        let symbols = &source.declarations().symbols;
        names.extend(
            symbols
                .member_owners(name)
                .filter(|owner| {
                    symbols
                        .member(owner, name)
                        .is_some_and(|member| matches!(member.kind, SymbolKind::Function(_)))
                })
                .map(str::to_owned),
        );
        for (owner, details) in symbols.type_declarations() {
            let base = details.alias.as_ref().or(details.underlying.as_ref());
            if !details.embedded.is_empty()
                || base.is_some_and(|r| {
                    matches!(r.kind, TypeRefKind::Named(_) | TypeRefKind::Inline(_))
                })
            {
                names.insert(owner.to_owned());
            }
        }
        // Anonymous interfaces can be API contracts too. Syntactic ordinals
        // survive identifier edits, including edits earlier on the same line.
        let mut ordinal = 0;
        let root = source.tree.root_node();
        let mut cursor = root.walk();
        loop {
            let node = cursor.node();
            if matches!(node.kind(), "interface_type" | "struct_type") {
                ordinal += 1;
                let top_level = node
                    .parent()
                    .filter(|n| is_type_declaration(*n))
                    .and_then(|n| n.parent())
                    .and_then(|n| n.parent())
                    .is_some_and(|n| n.kind() == "source_file");
                if !top_level
                    && let Some(ty) = resolver.source_type(package, source, node)
                    && let Some(method) = resolver.rename_method(&ty, name, declaring_package)?
                {
                    receivers.push(Receiver {
                        name: format!("{}#{ordinal}", file.display()),
                        ty,
                        method,
                    });
                }
            }
            if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    break;
                }
            }
            if cursor.node() == root {
                break;
            }
        }
    }
    for owner in names {
        let ty = TypeIdentity::named(package, &owner);
        if let Some(method) = resolver.rename_method(&ty, name, declaring_package)? {
            receivers.push(Receiver {
                name: owner,
                ty,
                method,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestProject;
    use crate::workspace::Workspace;
    use lsp_types::Position;
    use serde_json::json;

    fn method(text: &str, line: u32, name: &str, path: &std::path::Path) -> (PathBuf, Range) {
        let character = text.lines().nth(line as usize).unwrap().find(name).unwrap() as u32;
        (
            path.to_owned(),
            Range::new(
                Position::new(line, character),
                Position::new(line, character + name.len() as u32),
            ),
        )
    }

    #[test]
    fn joins_multiple_contracts_and_tracks_promoted_value_and_pointer_sets() {
        let project = TestProject::new();
        let text = "package sample\ntype I interface { Act(int) }\ntype J interface { Act(int); Extra() }\ntype K interface { I }\ntype V struct{}\nfunc(V) Act(int) {}\ntype P struct{}\nfunc(*P) Act(int) {}\nfunc(*P) Extra() {}\ntype W struct{ V }\ntype Other struct{}\nfunc(Other) Act(string) {}\n";
        let path = project.write("sample.go", text);
        let mut engine = Engine::new(project.root.clone());
        assert!(engine.install_workspace(
            engine.workspace_load(),
            project.workspace(&["sample.go"], true)
        ));
        let expected = [1, 2, 5, 7].map(|line| method(text, line, "Act", &path));
        for target in &expected {
            let contracts = method_contracts(&mut engine, target, "Act").unwrap();
            assert!(contracts.is_method);
            assert_eq!(contracts.family, expected);
            assert!(
                contracts
                    .relations
                    .iter()
                    .any(|edge| edge.implementer == "W" && !edge.pointer)
            );
            assert!(
                contracts
                    .relations
                    .iter()
                    .any(|edge| edge.implementer == "P" && edge.pointer)
            );
            assert!(
                !contracts
                    .relations
                    .iter()
                    .any(|edge| edge.implementer == "P" && !edge.pointer)
            );
        }
        let other = method(text, 11, "Act", &path);
        assert_eq!(
            method_contracts(&mut engine, &other, "Act").unwrap().family,
            vec![other]
        );
    }

    #[test]
    fn keeps_private_contracts_in_the_package_owners_build_group_and_test_view() {
        let project = TestProject::new();
        let regular = "package sample\ntype B struct{}\nfunc(B) mark() {}\n";
        let test = "package sample\ntype I interface{mark()}\nvar _ I = B{}\n";
        let path = project.write("b/b.go", regular);
        let test_path = project.write("b/b_test.go", test);
        project.write(
            "a/a.go",
            "package sample\ntype A struct{}\nfunc(A) mark() {}\n",
        );
        let folders = ["a", "b"].map(|name| project.root.join(name));
        // The regular ID collides across groups. Only b has tests, so its test
        // view keeps the unqualified ID and points to the qualified ForTest.
        let listing = [
            json!({"Dir":folders[0],"ImportPath":"test/dupe","Name":"sample","GoFiles":["a.go"]}),
            json!({"Dir":folders[1],"ImportPath":"1:test/dupe","Name":"sample","GoFiles":["b.go"]}),
            json!({"Dir":folders[1],"ImportPath":"test/dupe [test/dupe.test]","ForTest":"1:test/dupe","Name":"sample","GoFiles":["b.go","b_test.go"]}),
        ].map(|p| p.to_string()).join("\n");
        let workspace = Workspace::from_test_listing(&folders, &listing, true);
        let mut engine = Engine::with_roots(folders.to_vec());
        assert!(engine.install_workspace(engine.workspace_load(), workspace));
        let expected = [
            method(regular, 2, "mark", &path),
            method(test, 1, "mark", &test_path),
        ];
        for target in &expected {
            assert_eq!(
                method_contracts(&mut engine, target, "mark")
                    .unwrap()
                    .family,
                expected
            );
        }
    }

    #[test]
    fn refuses_a_method_contract_owned_by_a_dependency() {
        let project = TestProject::new();
        let dependency = TestProject::new();
        let text = "package sample\ntype T struct{}\nfunc(T) Read([]byte) (int, error) { return 0, nil }\n";
        let path = project.write("sample.go", text);
        dependency.write(
            "io.go",
            "package io\ntype Reader interface { Read([]byte) (int, error) }\n",
        );
        let listing = format!(
            "{}\n{}",
            json!({"Dir":project.root,"ImportPath":"test/sample","Name":"sample","GoFiles":["sample.go"],"Imports":["io"]}),
            json!({"Dir":dependency.root,"ImportPath":"io","Name":"io","Goroot":true,"GoFiles":["io.go"]})
        );
        let workspace =
            Workspace::from_test_listing(std::slice::from_ref(&project.root), &listing, true);
        let mut engine = Engine::new(project.root.clone());
        assert!(engine.install_workspace(engine.workspace_load(), workspace));
        let error =
            method_contracts(&mut engine, &method(text, 2, "Read", &path), "Read").unwrap_err();
        assert!(error.to_string().contains("read-only"), "{error}");
    }

    #[test]
    fn refuses_unresolved_contract_and_predeclared_error() {
        let project = TestProject::new();
        let text = "package sample\ntype T struct{}\nfunc(T) Act(int) {}\ntype I interface { Bad; Act(int) }\nfunc(T) Error() string { return \"\" }\n";
        let path = project.write("sample.go", text);
        let mut engine = Engine::new(project.root.clone());
        assert!(engine.install_workspace(
            engine.workspace_load(),
            project.workspace(&["sample.go"], true)
        ));
        for (line, name) in [(2, "Act"), (4, "Error")] {
            assert!(
                method_contracts(&mut engine, &method(text, line, name, &path), name).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn refuses_uncertain_sibling_method_signature() {
        let project = TestProject::new();
        let text = "package sample\ntype I interface{ Act(int); Extra(int) }\ntype T struct{}\nfunc (T) Act(int) {}\nfunc (T) Extra(Unknown) {}\n";
        let path = project.write("sample.go", text);
        let mut engine = Engine::new(project.root.clone());
        engine.install_workspace(
            engine.workspace_load(),
            project.workspace(&["sample.go"], true),
        );
        assert!(method_contracts(&mut engine, &method(text, 3, "Act", &path), "Act").is_err());
    }

    #[test]
    fn detects_a_named_field_displacing_an_existing_method_from_a_wrapper() {
        let project = TestProject::new();
        let before = "package sample\ntype E struct{}\nfunc (*E) Read() {}\ntype W struct{ *E; Other int }\ntype Wrap struct{ W }\n";
        let path = project.write("sample.go", before);
        let mut engine = Engine::new(project.root.clone());
        engine.install_workspace(
            engine.workspace_load(),
            project.workspace(&["sample.go"], true),
        );
        let old = field_destination(&mut engine, &method(before, 3, "Other", &path), "Read")
            .unwrap()
            .unwrap();
        let after = before.replace("Other", "Read");
        engine.open(path.clone(), after.clone(), 1).unwrap();
        let new = field_destination(&mut engine, &method(&after, 3, "Read", &path), "Read")
            .unwrap()
            .unwrap();
        assert!(old.receivers.iter().any(|(_, owner, _)| owner == "Wrap"));
        assert!(!new.receivers.iter().any(|(_, owner, _)| owner == "Wrap"));
        assert!(
            new.receivers
                .iter()
                .any(|(_, owner, pointer)| owner == "E" && *pointer)
        );
    }

    #[test]
    fn refuses_contracts_with_a_required_method_declared_on_an_alias_of_an_embedded_type() {
        let project = TestProject::new();
        let text = "package sample\ntype I interface{ Act(); Extra() }\ntype T struct{}\ntype B struct{}\ntype Alias = B\nfunc (Alias) Extra() {}\nfunc (T) Act() {}\ntype W struct { T; B }\n";
        let path = project.write("sample.go", text);
        let mut engine = Engine::new(project.root.clone());
        engine.install_workspace(
            engine.workspace_load(),
            project.workspace(&["sample.go"], true),
        );
        let outcome = method_contracts(&mut engine, &method(text, 6, "Act", &path), "Act");
        assert!(outcome.unwrap_err().to_string().contains("alias"));
    }

    #[test]
    fn rejects_unknown_embeddings_competing_with_a_promoted_method() {
        let project = TestProject::new();
        let text = "package sample\ntype I interface{ Act(int); Extra() }\ntype T struct{}\nfunc (T) Act(int) {}\ntype Wrapper struct { T; Missing }\nfunc (Wrapper) Extra() {}\n";
        let path = project.write("sample.go", text);
        let mut engine = Engine::new(project.root.clone());
        engine.install_workspace(
            engine.workspace_load(),
            project.workspace(&["sample.go"], true),
        );
        assert!(method_contracts(&mut engine, &method(text, 3, "Act", &path), "Act").is_err());
    }

    #[test]
    fn detects_lost_contract_when_a_field_shadows_the_promoted_new_method() {
        let project = TestProject::new();
        let before = "package sample\ntype I interface{ Act() }\ntype T struct{}\nfunc (T) Act() {}\ntype Wrapper struct{ T; Press int }\n";
        let path = project.write("sample.go", before);
        let mut engine = Engine::new(project.root.clone());
        engine.install_workspace(
            engine.workspace_load(),
            project.workspace(&["sample.go"], true),
        );
        let old = method_contracts(&mut engine, &method(before, 3, "Act", &path), "Act").unwrap();
        let after = before.replace("Act", "Press");
        engine.open(path.clone(), after.clone(), 1).unwrap();
        let new =
            method_contracts(&mut engine, &method(&after, 3, "Press", &path), "Press").unwrap();
        assert!(old.relations.iter().any(|r| r.implementer == "Wrapper"));
        assert!(!new.relations.iter().any(|r| r.implementer == "Wrapper"));
        assert_eq!(old.family.len(), new.family.len());
    }
}
