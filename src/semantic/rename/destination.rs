use super::*;
use crate::semantic::resolver::is_exported;

#[derive(PartialEq, Eq, Hash)]
struct Selection {
    namespace: String,
    package: String,
    receiver: String,
    pointer: bool,
}

#[derive(Default)]
pub(in crate::semantic::rename) struct MethodDestinations {
    selected: HashMap<Selection, Location>,
}

impl MethodDestinations {
    pub(in crate::semantic::rename) fn verify(
        &self,
        after: &Self,
        map: impl Fn(&Location) -> Location,
        name: &str,
    ) -> Result<()> {
        for (selection, original) in &self.selected {
            ensure!(
                after.selected.get(selection) == Some(&map(original)),
                "renaming would change existing method {name} on {}.{}",
                selection.package,
                selection.receiver
            );
        }
        Ok(())
    }
}

/// Record existing selections independently of whether a receiver selects the
/// old name. Renaming can introduce ambiguity even where that name was hidden.
pub(in crate::semantic::rename) fn method_destinations(
    engine: &mut Engine,
    family: &[Location],
    name: &str,
) -> Result<MethodDestinations> {
    let mut snapshot = engine.snapshot();
    let workspace = snapshot
        .workspace()
        .context("method rename requires an active package graph")?;
    let mut namespaces = BTreeMap::new();
    for (path, _) in family {
        let document = snapshot
            .document(path)?
            .context("missing method declaration document")?;
        let owner = snapshot
            .active_package(workspace, document.indexed())
            .context("method is outside the active package graph")?;
        namespaces.insert(owner.id.clone(), owner);
        // Exported methods use one namespace across Go packages.
        if is_exported(name) {
            break;
        }
    }
    let mut resolver = snapshot.resolver(workspace);
    let mut pending: Vec<_> = workspace
        .navigation_packages()
        .filter(|package| workspace.is_project_package(package))
        .collect();
    let mut seen = HashSet::new();
    let mut result = MethodDestinations::default();
    while let Some(package) = pending.pop() {
        if !seen.insert(&package.id) {
            continue;
        }
        pending.extend(workspace.direct_importers(package));
        for (namespace, owner) in &namespaces {
            let mut receivers = Vec::new();
            collect_receivers(&mut resolver, package, owner, name, &mut receivers)?;
            for receiver in receivers {
                let (path, range) = &receiver.method.location;
                let mut insert = |pointer| {
                    let selection = Selection {
                        namespace: namespace.clone(),
                        package: package.id.clone(),
                        receiver: receiver.name.clone(),
                        pointer,
                    };
                    result.selected.insert(selection, (path_key(path), *range));
                };
                if resolver.rename_value_has_method(&receiver.ty, &receiver.method)? {
                    insert(false);
                }
                if !receiver.method.is_interface() {
                    insert(true);
                }
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::{QueryProgress, SemanticTask};
    use crate::test_support::TestProject;

    fn check(types: &str) -> Result<()> {
        let text = format!(
            "package sample\ntype I interface {{ New(string) }}\ntype B struct{{}}\nfunc (*B) New(string) {{}}\n{types}\n"
        );
        rename(&text, "Old(int)", "New").map(|_| ())
    }

    fn rename(text: &str, selection: &str, new_name: &str) -> Result<lsp_types::WorkspaceEdit> {
        let project = TestProject::new();
        let path = project.write("sample.go", text);
        let mut engine = Engine::new(project.root.clone());
        engine.install_workspace(
            engine.workspace_load(),
            project.workspace(&["sample.go"], true),
        );
        let start = text.find(selection).unwrap();
        let position = engine
            .snapshot()
            .document(&path)?
            .unwrap()
            .byte_range(start..start)
            .start;
        let result = match engine.rename(&path, position, new_name)? {
            QueryProgress::Ready(result) => result,
            QueryProgress::Searching(mut task) => {
                while !task.advance(&mut engine)? {}
                task.finish()
            }
            QueryProgress::Pending(_) => panic!("test package graph is loaded"),
        };
        result.map_err(|error| anyhow::anyhow!("{error:?}"))
    }

    #[test]
    fn preserves_an_existing_promoted_contract_without_a_selector_use() {
        let result = check("type W struct{ *B }\nfunc (W) Old(int){}\nvar _ I = W{}");
        assert!(result.unwrap_err().to_string().contains("New"));
    }

    #[test]
    fn rejects_ambiguity_in_a_wrapper_embedding_both_method_owners() {
        let result =
            check("type A struct{}\nfunc (A) Old(int){}\ntype W struct{ A; *B }\nvar _ I = W{}");
        assert!(result.unwrap_err().to_string().contains("New"));
    }

    #[test]
    fn permits_a_destination_method_on_an_unrelated_receiver() {
        check("type A struct{}\nfunc (*A) Old(int){}\nvar _ I = (*B)(nil)").unwrap();
    }

    #[test]
    fn protects_a_destination_when_the_old_selector_was_ambiguous() {
        let result = check(
            "type A struct{}\nfunc (A) Old(int){}\ntype Other struct{}\nfunc (Other) Old(uint){}\ntype W struct{ A; Other; *B }\nvar _ I = W{}",
        );
        assert!(result.unwrap_err().to_string().contains("New"));
    }

    #[test]
    fn protects_a_destination_when_the_old_method_was_shadowed() {
        let result = check(
            "type A struct{}\nfunc (A) Old(int){}\ntype W struct{ A; *B }\nfunc (W) Old(uint){}\nvar _ I = W{}",
        );
        assert!(result.unwrap_err().to_string().contains("New"));
    }

    #[test]
    fn renames_a_method_together_with_an_anonymous_interface_parameter() {
        let text = "package sample\ntype S struct{}\nfunc (S) M() {}\nfunc take(v interface{ M() }) { v.M() }\nfunc use() { take(S{}) }\n";
        let edit = rename(text, "M() {}", "Do").unwrap();
        let document = serde_json::to_value(edit).unwrap();
        let edits = document["documentChanges"][0]["edits"].as_array().unwrap();
        assert_eq!(edits.len(), 3, "{edits:?}");
    }

    #[test]
    fn preserves_a_destination_selected_on_anonymous_struct_and_interface_types() {
        let text = "package sample\ntype A struct{}\nfunc(A) Old(int){}\ntype B struct{}\nfunc(B) New(string){}\ntype C struct{}\nfunc(C) Old(uint){}\nfunc take(v interface{New(string)}){}\nfunc use(){take(struct{A;B;C}{})}\n";
        assert!(
            rename(text, "Old(int)", "New")
                .unwrap_err()
                .to_string()
                .contains("New")
        );
    }

    #[test]
    fn allows_a_wrapper_to_keep_its_direct_method_over_a_promoted_rename() {
        check("type A struct{}\nfunc (A) Old(int){}\ntype W struct{ A }\nfunc(W) New(string){}\nvar _ I = W{}").unwrap();
    }
}
