use super::{Engine, Location};
use crate::scope::lexical_binding;
use crate::semantic::resolver::{
    Resolver, TypeKind, is_exported, is_predeclared_type, materialize_type,
};
use crate::semantic::snapshot::EngineSnapshot;
use crate::semantic::source::{Source, node_text};
use crate::symbols::{InlineType, TypeRef, TypeRefKind, type_ref};
use crate::workspace::{Package, Workspace, path_key};
use anyhow::{Context, Result, ensure};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tree_sitter::Node;

/// Field names take part in Go's structural type identity, including struct
/// conversions that ignore tags. These relationships have no selector use.
struct Layout {
    shape: Arc<InlineType>,
    named_fields: Vec<Option<(u8, FieldType)>>,
}

#[derive(PartialEq)]
enum FieldType {
    Builtin(String),
    Defined(PathBuf, String),
}

fn field_type<'w>(
    resolver: &mut Resolver<'w, '_>,
    package: &'w Package,
    source: &Source,
    node: Node<'_>,
    field: Option<&TypeRef>,
) -> Result<Option<(u8, FieldType)>> {
    let Some(field) = field else { return Ok(None) };
    let TypeRefKind::Named(original) = &field.kind else {
        return Ok(None);
    };
    if original.qualifier.is_none() && lexical_binding(node, &original.name, &source.text).is_some()
    {
        return Ok(None); // A local type or type parameter needs its lexical context.
    }
    let Some(ty) = materialize_type(
        resolver.workspace,
        package,
        &source.path,
        &source.imports,
        field.clone(),
        &[],
    ) else {
        return Ok(None);
    };
    let Some(ty) = resolver.resolve_alias(ty)? else {
        return Ok(None);
    };
    let TypeKind::Named { name, .. } = &ty.kind else {
        return Ok(None);
    };
    let identity = if let Some((file, _)) = resolver.type_info(ty.package, name)? {
        // Distinct definitions remain different even when their underlying types
        // match. Instantiations of one generic definition may still coincide.
        FieldType::Defined(path_key(&file), name.clone())
    } else if is_predeclared_type(name) {
        FieldType::Builtin(match name.as_str() {
            "byte" => "uint8".into(),
            "rune" => "int32".into(),
            _ => name.clone(),
        })
    } else {
        return Ok(None);
    };
    Ok(Some((ty.pointers, identity)))
}

struct Named {
    name: String,
    location: Location,
}

fn defined_struct(source: &Source, node: Node<'_>) -> Option<Named> {
    let spec = node
        .parent()
        .filter(|s| s.kind() == "type_spec" && s.child_by_field_name("type") == Some(node))?;
    let name = spec.child_by_field_name("name")?;
    Some(Named {
        name: node_text(name, &source.text).into(),
        location: (path_key(&source.path), source.range(name)),
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Uses {
    Unused,
    Ordinary,
    Unknown,
}

// Two defined types retain their distinct identities. A conversion, alias,
// derived type or generic constraint may still depend on their underlying
// structs. Only literal construction and plain type annotations prove otherwise.
fn ordinary_type_use(mut node: Node<'_>) -> bool {
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "qualified_type" if parent.child_by_field_name("name") == Some(node) => node = parent,
            "slice_type" | "array_type" | "pointer_type" | "map_type" | "parenthesized_type" => {
                node = parent
            }
            "composite_literal"
            | "parameter_declaration"
            | "variadic_parameter_declaration"
            | "var_spec" => {
                return parent.child_by_field_name("type") == Some(node);
            }
            _ => return false,
        }
    }
    false
}

fn uses<'w>(
    snapshot: &mut EngineSnapshot<'w>,
    workspace: &'w Workspace,
    named: Option<&Named>,
    files: &[&PathBuf],
    cache: &mut HashMap<Location, Uses>,
) -> Result<Uses> {
    let Some(named) = named else {
        return Ok(Uses::Unknown);
    };
    if let Some(&uses) = cache.get(&named.location) {
        return Ok(uses);
    }
    let mut uses = Uses::Unused;
    'files: for &file in files {
        if !snapshot.docs().contains_key(file)
            && !std::fs::read_to_string(file)?.contains(&named.name)
        {
            continue;
        }
        let document = snapshot
            .document(file)?
            .context("missing struct type usage input")?;
        let source = document.indexed();
        let Some(package) = snapshot.active_package(workspace, source) else {
            continue;
        };
        let root = snapshot.root();
        let mut resolver = snapshot.resolver(workspace);
        for offset in source.name_offsets(&named.name) {
            let Some(reference) = source.reference_at_offset(*offset) else {
                continue;
            };
            let node = reference.name();
            if path_key(file) == named.location.0 && source.range(node) == named.location.1 {
                continue;
            }
            match super::target::binding(&mut resolver, package, source, reference, root)? {
                Some((path, range))
                    if range != named.location.1 || path_key(&path) != named.location.0 =>
                {
                    continue;
                }
                Some(_) if ordinary_type_use(node) => uses = Uses::Ordinary,
                _ => {
                    uses = Uses::Unknown;
                    break 'files;
                }
            }
        }
    }
    cache.insert(named.location.clone(), uses);
    Ok(uses)
}

impl Layout {
    fn new<'w>(
        resolver: &mut Resolver<'w, '_>,
        package: &'w Package,
        source: &Source,
        node: Node<'_>,
    ) -> Result<Self> {
        let Some(TypeRefKind::Inline(shape)) =
            type_ref(node, &source.text, &|n| source.range(n)).map(|ty| ty.kind)
        else {
            anyhow::bail!("cannot verify the struct type in {}", source.path.display());
        };
        let named_fields = shape
            .fields
            .iter()
            .map(|field| field_type(resolver, package, source, node, field.ty.as_ref()))
            .collect::<Result<_>>()?;
        Ok(Self {
            shape,
            named_fields,
        })
    }

    fn could_match(&self, other: &Self, old: &str, replacement: &str, same_package: bool) -> bool {
        self.shape.fields.len() == other.shape.fields.len()
            && self
                .shape
                .fields
                .iter()
                .zip(&other.shape.fields)
                .enumerate()
                .all(|(index, (a, b))| {
                    let name = if a.name == old { replacement } else { &a.name };
                    name == b.name
                        && (same_package || is_exported(name))
                        && a.embedded == b.embedded
                        && !matches!((&self.named_fields[index], &other.named_fields[index]),
                        (Some(left), Some(right)) if left != right)
                })
    }
}

fn enclosing_struct(node: Node<'_>) -> Option<Node<'_>> {
    let field = std::iter::successors(Some(node), |n| n.parent())
        .find(|n| n.kind() == "field_declaration")?;
    field
        .parent()?
        .parent()
        .filter(|n| n.kind() == "struct_type")
}

pub(super) fn check(
    engine: &mut Engine,
    field: &Location,
    new_name: &str,
    linked_fields: &[Location],
    dependencies: &HashMap<PathBuf, u64>,
) -> Result<()> {
    let mut snapshot = engine.snapshot();
    let workspace = snapshot.workspace().context("missing package graph")?;
    let declaration = snapshot
        .document(&field.0)?
        .context("missing field declaration")?;
    let source = declaration.indexed();
    let name = source
        .reference_at(field.1.start)
        .context("missing field name")?
        .name();
    let old = node_text(name, &source.text);
    let node = enclosing_struct(name).context("missing struct for field declaration")?;
    let owner = snapshot
        .active_package(workspace, source)
        .context("missing field package")?;
    let layout = Layout::new(&mut snapshot.resolver(workspace), owner, source, node)?;
    let named = defined_struct(source, node);
    let origin = (path_key(&field.0), source.range(node));
    // All captured project files and open dependencies can introduce new import
    // edges. Closed packages importing any of these can see their exposed types.
    let documents = snapshot.docs();
    let mut importers = HashSet::new();
    let mut pending: Vec<_> = documents
        .values()
        .filter_map(|s| snapshot.active_package(workspace, s))
        .collect();
    while let Some(package) = pending.pop() {
        if importers.insert(&package.id) {
            pending.extend(workspace.direct_importers(package));
        }
    }
    let files: Vec<_> = documents.keys().chain(dependencies.keys()).collect();
    let mut peers = Vec::new();
    for &file in &files {
        let package = documents
            .get(file)
            .and_then(|s| snapshot.active_package(workspace, s))
            .or_else(|| workspace.package_of_file(file));
        let Some(package) = package else { continue };
        let same_package = owner.directory_and_name() == package.directory_and_name();
        let check_literals = !same_package
            && is_exported(old)
            && !is_exported(new_name)
            && importers.contains(&package.id);
        if !check_literals && dependencies.contains_key(file) {
            let text = std::fs::read_to_string(file)?;
            if !text.contains("struct") || (!text.contains(old) && !text.contains(new_name)) {
                continue;
            }
        }
        let document = snapshot
            .document(file)?
            .context("missing field rename input")?;
        let source = document.indexed();
        let mut resolver = snapshot.resolver(workspace);
        for spelling in [old, new_name] {
            for offset in source.name_offsets(spelling) {
                let Some(reference) = source.reference_at_offset(*offset) else {
                    continue;
                };
                let field_node = reference.name();
                if !(field_node.kind() == "field_identifier"
                    && field_node
                        .parent()
                        .is_some_and(|p| p.kind() == "field_declaration"))
                    && !super::scan::embedded_type_use(field_node)
                {
                    continue;
                }
                let Some(other) = enclosing_struct(field_node) else {
                    continue;
                };
                if path_key(file) == origin.0 && source.range(other) == origin.1 {
                    continue;
                }
                if linked_fields
                    .iter()
                    .any(|(p, r)| *r == source.range(field_node) && path_key(p) == path_key(file))
                {
                    continue;
                }
                let peer = Layout::new(&mut resolver, package, source, other)?;
                if layout.could_match(&peer, old, spelling, same_package) {
                    peers.push((
                        defined_struct(source, other),
                        file,
                        source.range(other).start.line + 1,
                    ));
                }
            }
        }
        if check_literals {
            check_positional_literals(&mut resolver, package, source, field, old)?;
        }
    }
    if !peers.is_empty() {
        let mut cache = HashMap::new();
        let original = uses(&mut snapshot, workspace, named.as_ref(), &files, &mut cache)?;
        for (peer, file, line) in peers {
            let other = uses(&mut snapshot, workspace, peer.as_ref(), &files, &mut cache)?;
            ensure!(
                original == Uses::Unused
                    || other == Uses::Unused
                    || (original == Uses::Ordinary && other == Uses::Ordinary),
                "renaming field {old} could change struct type identity at {}:{line}",
                file.display()
            );
        }
    }
    Ok(())
}

fn check_positional_literals<'w>(
    resolver: &mut Resolver<'w, '_>,
    package: &'w Package,
    source: &Source,
    field: &(PathBuf, lsp_types::Range),
    name: &str,
) -> Result<()> {
    let mut pending = vec![source.tree.root_node()];
    while let Some(node) = pending.pop() {
        pending.extend(node.named_children(&mut node.walk()));
        if node.kind() != "literal_value"
            || !node
                .named_children(&mut node.walk())
                .any(|n| n.kind() == "literal_element")
        {
            continue;
        }
        let ty = resolver
            .literal_type(package, source, node, 0)?
            .with_context(|| {
                format!(
                    "cannot verify positional literal in {}",
                    source.path.display()
                )
            })?;
        let ty = resolver
            .underlying_type(ty)?
            .context("cannot verify positional literal type")?;
        ensure!(
            !matches!(&ty.kind, TypeKind::Parameter(_))
                && !matches!(&ty.kind, TypeKind::Inline {shape, ..} if shape.interface),
            "cannot verify positional literal with a type parameter in {}",
            source.path.display()
        );
        if let Some(member) = resolver.direct_member(&ty, name, false)? {
            ensure!(
                member.symbol.range != field.1 || path_key(&member.file) != path_key(&field.0),
                "unexporting field {name} would break a positional literal at {}:{}",
                source.path.display(),
                source.range(node).start.line + 1
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::test_support::TestProject;
    use crate::workspace::Workspace;
    use serde_json::json;

    fn rename(declarations: &str, consumer: &str, selected: &str, new_name: &str) -> RenameResult {
        let project = TestProject::new();
        let src = format!("package sample\n{declarations}\n");
        let path = project.write("sample.go", &src);
        project.write("client/use.go", &format!("package client\n{consumer}\n"));
        let listing = format!(
            "{}\n{}",
            json!({"Dir":project.root,"ImportPath":"test/sample","Name":"sample","GoFiles":["sample.go"]}),
            json!({"Dir":project.root.join("client"),"ImportPath":"test/client","Name":"client","GoFiles":["use.go"],"Imports":["test/sample"]})
        );
        let mut engine = Engine::new(project.root.clone());
        engine.install_workspace(
            engine.workspace_load(),
            Workspace::from_test_listing(std::slice::from_ref(&project.root), &listing, true),
        );
        request(&mut engine, &path, &src, selected, new_name)
    }

    fn request(
        engine: &mut Engine,
        path: &Path,
        src: &str,
        selected: &str,
        new_name: &str,
    ) -> RenameResult {
        let at = engine
            .snapshot()
            .document(path)
            .unwrap()
            .unwrap()
            .indexed()
            .lines
            .position(src, src.find(selected).unwrap());
        match engine.rename(path, at, new_name).unwrap() {
            QueryProgress::Ready(result) => result,
            QueryProgress::Searching(mut task) => {
                while !task.advance(engine).unwrap() {}
                task.finish()
            }
            QueryProgress::Pending(_) => panic!("package graph is installed"),
        }
    }

    #[test]
    fn refuses_unexporting_a_field_assigned_positionally_in_another_package() {
        for usage in ["sample.T{1}", "[]sample.T{{1}}", "Alias{1}", "Derived{1}"] {
            let consumer = format!(
                "import \"test/sample\"\ntype Alias = sample.T\ntype Derived sample.T\nvar _ = {usage}"
            );
            assert!(
                matches!(
                    rename("type T struct{ X int }", &consumer, "X int", "x"),
                    Err(RenameError::Rejected(_))
                ),
                "{usage}"
            );
        }
    }

    #[test]
    fn rejects_field_renames_that_change_structural_assignments_and_method_signatures() {
        for source in [
            "type T struct{ X int }; var _ T = struct{ X int }{}",
            "type I interface{ M(struct{ X int }) }; type T struct{}; func(T) M(struct{ X int }){}; var _ I = T{}",
            "type T struct{ X int }; type U struct{ X int }; var _ = U(T{})",
            "type T struct{ X int }; func Check[S ~struct{ X int }](v S) {}; func f(){Check(T{})}",
        ] {
            assert!(
                matches!(
                    rename(source, "", "X int", "Y"),
                    Err(RenameError::Rejected(_))
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn permits_unrelated_literals_and_distinct_field_types() {
        let source =
            "type T struct{ X int }; var _ = T{1}; type U struct{ X string }; var _ = U{\"\"}";
        let consumer = "import \"test/sample\"\nvar _ = sample.T{}; var _ = sample.U{\"\"}";
        rename(source, consumer, "X int", "x").unwrap();
    }

    #[test]
    fn distinguishes_a_defined_field_type_from_a_builtin_and_resolves_aliases() {
        for definition in ["type ID struct{}", "type ID int"] {
            let src = format!(
                "{definition}; type T struct{{ X int }}; type U struct{{ X ID }}; type AT = T; type AU = U"
            );
            rename(&src, "", "X int", "Y").unwrap();
        }
        let src = "type ID = int; type T struct{ X int }; type U struct{ X ID }; var _ = U(T{})";
        assert!(matches!(
            rename(src, "", "X int", "Y"),
            Err(RenameError::Rejected(_))
        ));
    }

    #[test]
    fn permits_identically_shaped_named_types_with_independent_uses() {
        for uses in [
            "var _ = A{X: 1}.X",
            "func f(a A, b B) int { return a.X + b.X }; var _ = f(A{X: 1}, B{X: 2})",
            "var _ = []A{{X: 1}}; var _ = B{X: 2}.X",
        ] {
            let src = format!("type A struct{{ X int }}; type B struct{{ X int }}; {uses}");
            rename(&src, "", "X int", "Y").unwrap();
        }
    }

    #[test]
    fn preserves_a_conversion_when_both_implicit_fields_follow_the_same_type() {
        let src = "type Inner struct{}; type A struct{Inner}; type B struct{Inner}; var _ = B(A{})";
        rename(src, "", "Inner struct", "Renamed").unwrap();
    }

    #[test]
    fn checks_matching_structs_from_closed_and_open_dependencies() {
        let project = TestProject::new();
        let dep = TestProject::new();
        let src =
            "package sample\nimport \"test/dep\"\ntype T struct{ Old int }; var _ = dep.S(T{})";
        let path = project.write("sample.go", src);
        let external = "package dep\ntype S struct{ Old int }";
        let external_path = dep.write("dep.go", external);
        let listing = format!(
            "{}\n{}",
            json!({"Dir":project.root,"ImportPath":"test/sample","Name":"sample","GoFiles":["sample.go"],"Imports":["test/dep"]}),
            json!({"Dir":dep.root,"ImportPath":"test/dep","Name":"dep","GoFiles":["dep.go"]})
        );
        let mut engine = Engine::new(project.root.clone());
        engine.install_workspace(
            engine.workspace_load(),
            Workspace::from_test_listing(std::slice::from_ref(&project.root), &listing, true),
        );
        assert!(
            matches!(
                request(&mut engine, &path, src, "Old int", "New"),
                Err(RenameError::Rejected(_))
            ),
            "closed dependency"
        );
        engine.open(external_path, external.into(), 1).unwrap();
        assert!(
            matches!(
                request(&mut engine, &path, src, "Old int", "New"),
                Err(RenameError::Rejected(_))
            ),
            "open dependency"
        );
    }
}
