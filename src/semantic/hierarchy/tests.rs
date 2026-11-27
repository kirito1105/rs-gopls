use std::path::{Path, PathBuf};

use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, FileChangeType,
    Position, Range, SymbolKind, TextDocumentContentChangeEvent, TypeHierarchyItem,
};
use serde::Deserialize;
use serde_json::json;

use super::call::{incoming_calls, outgoing_calls, prepare_call_hierarchy};
use super::type_hierarchy::{prepare_type_hierarchy, subtypes, supertypes};
use crate::protocol::{file_path, path_uri};
use crate::semantic::{Engine, QueryProgress, SemanticTask};
use crate::test_support::TestProject;
use crate::workspace::{Workspace, path_key};

const MANIFEST: &str = include_str!("../../../tools/fixtures/hierarchy/hierarchy-queries.json");

#[derive(Deserialize)]
struct Manifest {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    path: PathBuf,
    line: u32,
    character: u32,
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tools")
        .join("fixtures")
        .join("hierarchy")
}

fn fixture_engine() -> Engine {
    let root = fixture_root();
    let mut engine = Engine::new(root.clone());
    assert!(engine.install_workspace(
        engine.workspace_load(),
        Workspace::load(&root, true).unwrap()
    ));
    engine
}

fn case(id: &str) -> (PathBuf, Position) {
    let manifest: Manifest = serde_json::from_str(MANIFEST).unwrap();
    let case = manifest
        .cases
        .into_iter()
        .find(|case| case.id == id)
        .unwrap();
    (
        fixture_root().join(case.path),
        Position::new(case.line, case.character),
    )
}

fn complete<T, Task: SemanticTask<T>>(engine: &mut Engine, progress: QueryProgress<T, Task>) -> T {
    match progress {
        QueryProgress::Ready(result) => result,
        QueryProgress::Searching(mut task) => {
            while !task.advance(engine).unwrap() {}
            task.finish()
        }
        QueryProgress::Pending(_) => panic!("expected the installed workspace"),
    }
}

fn single<T>(progress: QueryProgress<Option<Vec<T>>>) -> T {
    let QueryProgress::Ready(Some(items)) = progress else {
        panic!("missing hierarchy target")
    };
    let Ok([item]) = <[T; 1]>::try_from(items) else {
        panic!("expected one hierarchy target")
    };
    item
}

fn call(engine: &mut Engine, id: &str) -> CallHierarchyItem {
    let (path, pos) = case(id);
    single(prepare_call_hierarchy(engine, &path, pos).unwrap())
}

fn ty(engine: &mut Engine, id: &str) -> TypeHierarchyItem {
    let (path, pos) = case(id);
    single(prepare_type_hierarchy(engine, &path, pos).unwrap())
}

fn callers(engine: &mut Engine, item: &CallHierarchyItem) -> Vec<CallHierarchyIncomingCall> {
    let progress = incoming_calls(engine, item).unwrap();
    complete(engine, progress).unwrap()
}

fn callees(engine: &mut Engine, item: &CallHierarchyItem) -> Vec<CallHierarchyOutgoingCall> {
    let progress = outgoing_calls(engine, item).unwrap();
    complete(engine, progress).unwrap()
}

fn span(line: u32, start: u32, end: u32) -> Range {
    Range::new(Position::new(line, start), Position::new(line, end))
}

#[test]
fn fixture_calls_group_utf16_sites_by_declaring_function_and_method() {
    let mut engine = fixture_engine();
    let dispatch = call(&mut engine, "prepare-function");
    let leaf = call(&mut engine, "prepare-cross-package-call");
    let close = call(&mut engine, "prepare-interface-call");
    let run = call(&mut engine, "prepare-method-call");
    assert_eq!(dispatch.kind, SymbolKind::FUNCTION);
    assert_eq!(dispatch.detail.as_deref(), Some("hierarchy"));
    assert_eq!(dispatch.selection_range, span(4, 5, 13));
    assert_eq!(
        dispatch.range,
        Range::new(Position::new(4, 0), Position::new(7, 1))
    );
    assert_eq!(leaf, call(&mut engine, "leaf-callers"));
    assert_eq!(leaf.selection_range, span(41, 5, 9));
    assert_eq!(leaf.detail.as_deref(), Some("worker"));
    assert_eq!(leaf.uri, path_uri(&case("leaf-callers").0).unwrap());
    assert_eq!(
        leaf.data,
        Some(json!({
            "path": file_path(&leaf.uri).unwrap(), "selectionStart": leaf.selection_range.start, "kind": "call"
        }))
    );
    assert_eq!(
        (close.name.as_str(), close.kind),
        ("ReadCloser.Close", SymbolKind::METHOD)
    );
    assert_eq!(
        (run.name.as_str(), run.kind),
        ("(Derived).Run", SymbolKind::METHOD)
    );
    assert_eq!(close.selection_range, span(8, 1, 6));
    assert_eq!(close.range, span(8, 1, 8));

    let incoming = callers(&mut engine, &leaf);
    assert_eq!(
        incoming
            .iter()
            .map(|call| (call.from.name.as_str(), call.from_ranges.as_slice()))
            .collect::<Vec<_>>(),
        [
            ("Dispatch", &[span(6, 48, 52), span(6, 73, 77)][..]),
            ("TestLeaf", &[span(17, 24, 28)]),
            ("Recursive", &[span(47, 9, 13), span(49, 8, 12)]),
            ("(Derived).Run", &[span(53, 8, 12)]),
        ]
    );
    let outgoing = callees(&mut engine, &dispatch);
    assert_eq!(
        outgoing
            .iter()
            .map(|call| (call.to.name.as_str(), call.from_ranges.as_slice()))
            .collect::<Vec<_>>(),
        [
            ("Reader.Read", &[span(6, 16, 20)][..]),
            ("ReadCloser.Close", &[span(5, 9, 14)]),
            ("Leaf", &[span(6, 48, 52), span(6, 73, 77)]),
            ("(Derived).Run", &[span(6, 33, 36)]),
        ]
    );
    let recursive = call(&mut engine, "recursive-callers");
    let incoming = callers(&mut engine, &recursive);
    assert_eq!(
        incoming
            .iter()
            .map(|call| call.from.name.as_str())
            .collect::<Vec<_>>(),
        ["Recurse", "Recursive"]
    );
    assert_eq!(incoming[1].from_ranges, [span(49, 22, 31)]);
    let outgoing = callees(&mut engine, &recursive);
    assert_eq!(outgoing[0].to, leaf);
    assert_eq!(outgoing[0].from_ranges, [span(47, 9, 13), span(49, 8, 12)]);
    assert_eq!(outgoing[1].to, recursive);
    assert_eq!(outgoing[1].from_ranges, incoming[1].from_ranges);
    assert_eq!(outgoing.len(), 2);
    let read = call(&mut engine, "interface-method-callers");
    assert_eq!(
        callers(&mut engine, &read),
        [CallHierarchyIncomingCall {
            from: dispatch,
            from_ranges: vec![span(6, 16, 20)]
        }]
    );
    assert_eq!(
        callees(&mut engine, &run)
            .iter()
            .map(|call| call.to.name.as_str())
            .collect::<Vec<_>>(),
        ["(Base).Read", "Leaf"]
    );
    assert!(callees(&mut engine, &read).is_empty());
    assert!(callees(&mut engine, &leaf).is_empty());
}

fn project_names(items: Vec<TypeHierarchyItem>) -> Vec<String> {
    assert!(
        items
            .windows(2)
            .all(|pair| (&pair[0].uri, pair[0].selection_range.start)
                < (&pair[1].uri, pair[1].selection_range.start))
    );
    let root = path_key(&fixture_root());
    items
        .into_iter()
        .filter(|item| path_key(&file_path(&item.uri).unwrap()).starts_with(&root))
        .map(|item| item.name)
        .collect()
}

fn parents(engine: &mut Engine, item: &TypeHierarchyItem) -> Vec<String> {
    let progress = supertypes(engine, item).unwrap();
    project_names(complete(engine, progress).unwrap())
}

fn children(engine: &mut Engine, item: &TypeHierarchyItem) -> Vec<String> {
    let progress = subtypes(engine, item).unwrap();
    project_names(complete(engine, progress).unwrap())
}

#[test]
fn fixture_types_combine_implementations_with_direct_embedding() {
    let mut engine = fixture_engine();
    let reader = ty(&mut engine, "reader-subtypes");
    let read_closer = ty(&mut engine, "read-closer-subtypes");
    let service = ty(&mut engine, "prepare-interface");
    let base = ty(&mut engine, "base-subtypes");
    let derived = ty(&mut engine, "prepare-struct");
    let alias = ty(&mut engine, "prepare-alias-use");
    assert_eq!(
        (service.name.as_str(), service.kind),
        ("Service", SymbolKind::INTERFACE)
    );
    assert_eq!(
        (derived.name.as_str(), derived.kind),
        ("Derived", SymbolKind::STRUCT)
    );
    assert_eq!(
        (alias.name.as_str(), alias.kind),
        ("Alias", SymbolKind::CLASS)
    );
    assert_eq!(alias.range, span(36, 0, 20));
    assert_eq!(alias.selection_range, span(36, 5, 10));
    assert_eq!(alias.detail.as_deref(), Some("worker"));
    assert_eq!(
        alias.data,
        Some(json!({
            "path": file_path(&alias.uri).unwrap(), "selectionStart": alias.selection_range.start, "kind": "type"
        }))
    );
    let path = fixture_root().join("hierarchy.go");
    assert_eq!(
        single(prepare_type_hierarchy(&mut engine, &path, Position::new(6, 26)).unwrap()),
        alias
    );
    assert_eq!(
        single(prepare_type_hierarchy(&mut engine, &path, Position::new(5, 2)).unwrap()),
        service
    );
    assert_eq!(parents(&mut engine, &reader), Vec::<String>::new());
    assert_eq!(parents(&mut engine, &read_closer), ["Reader"]);
    assert_eq!(parents(&mut engine, &service), ["ReadCloser"]);
    assert_eq!(
        parents(&mut engine, &derived),
        ["Reader", "ReadCloser", "Service", "Resetter", "Base"]
    );
    assert_eq!(parents(&mut engine, &alias), parents(&mut engine, &derived));
    assert_eq!(
        children(&mut engine, &reader),
        ["ReadCloser", "Base", "Derived", "Alias"]
    );
    assert_eq!(
        children(&mut engine, &read_closer),
        ["Service", "Derived", "Alias"]
    );
    assert_eq!(children(&mut engine, &base), ["Derived"]);
    assert!(children(&mut engine, &derived).is_empty());
}

fn at(text: &str, marker: &str) -> Position {
    let prefix = &text[..text.find(marker).unwrap()];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let column = prefix.rsplit('\n').next().unwrap().encode_utf16().count() as u32;
    Position::new(line, column)
}

#[test]
fn unsaved_calls_and_embedded_types_update_and_revert_on_close() {
    let mut engine = fixture_engine();
    let leaf = call(&mut engine, "leaf-callers");
    let base = ty(&mut engine, "base-subtypes");
    let reader = ty(&mut engine, "reader-subtypes");
    let path = fixture_root().join("draft.go");
    let text = concat!(
        "package hierarchy\nimport w \"example.test/hierarchy/worker\"\n",
        "type Wrapped struct { *w.Base }\ntype Child interface { w.Reader }\n",
        "type NamedField struct { field w.Base }\nvar global = w.Leaf\n",
        "func identity[T any](x T) T { return x }\n",
        "func Extra() w.Base {\n",
        "  _ = w.Leaf\n  func() { _ = \"🙂\"; w.Leaf(1); w.Leaf(2) }()\n",
        "  _ = identity[int](1); _ = int(1); _ = len(\"x\")\n",
        "  return w.Base{}\n}\n",
        "func use() { _ = Extra() }\n",
    );
    engine.open(path.clone(), text.into(), 1).unwrap();
    let extra = single(prepare_call_hierarchy(&mut engine, &path, at(text, "Extra() w")).unwrap());
    let incoming = callers(&mut engine, &leaf);
    let group = incoming.iter().find(|call| call.from == extra).unwrap();
    assert_eq!(group.from_ranges.len(), 3);
    assert_eq!(group.from_ranges[0].start, at(text, "Leaf\n  func"));
    let outgoing = callees(&mut engine, &extra);
    assert_eq!(
        outgoing
            .iter()
            .map(|call| call.to.name.as_str())
            .collect::<Vec<_>>(),
        ["identity", "Leaf"]
    );
    assert_eq!(outgoing[1].from_ranges, group.from_ranges[1..]);
    assert_eq!(
        outgoing[0].to,
        single(prepare_call_hierarchy(&mut engine, &path, at(text, "identity[int]")).unwrap())
    );
    assert!(matches!(
        prepare_call_hierarchy(&mut engine, &path, at(text, "len(")).unwrap(),
        QueryProgress::Ready(None)
    ));
    let wrapped = single(prepare_type_hierarchy(&mut engine, &path, at(text, "Wrapped")).unwrap());
    let child = single(prepare_type_hierarchy(&mut engine, &path, at(text, "Child")).unwrap());
    assert_eq!(parents(&mut engine, &wrapped), ["Child", "Reader", "Base"]);
    assert_eq!(parents(&mut engine, &child), ["Reader"]);
    assert_eq!(children(&mut engine, &base), ["Wrapped", "Derived"]);
    assert!(children(&mut engine, &reader).contains(&"Child".to_owned()));
    let mut closing = at(text, "Extra() }");
    closing.character += "Extra(".len() as u32;
    assert_eq!(
        single(prepare_type_hierarchy(&mut engine, &path, closing).unwrap()),
        base
    );

    let updated = text
        .replace("w.Leaf", "w.Recursive")
        .replace("*w.Base", "field w.Base")
        .replace("interface { w.Reader }", "interface { w.Resetter }");
    engine
        .edit(
            &path,
            &[TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: updated,
            }],
            2,
        )
        .unwrap();
    assert!(
        !callers(&mut engine, &leaf)
            .iter()
            .any(|call| call.from == extra)
    );
    assert!(
        callees(&mut engine, &extra)
            .iter()
            .any(|call| call.to.name == "Recursive" && call.from_ranges.len() == 2)
    );
    assert_eq!(children(&mut engine, &base), ["Derived"]);
    assert!(!children(&mut engine, &reader).contains(&"Child".to_owned()));
    assert_eq!(parents(&mut engine, &child), ["Resetter"]);
    engine.close(&path);
    assert_eq!(children(&mut engine, &base), ["Derived"]);
    assert_eq!(callers(&mut engine, &leaf).len(), 4);
    assert!(matches!(
        outgoing_calls(&mut engine, &extra).unwrap(),
        QueryProgress::Ready(None)
    ));
    assert!(matches!(
        subtypes(&mut engine, &wrapped).unwrap(),
        QueryProgress::Ready(None)
    ));
}

#[test]
fn external_test_package_resolves_generic_calls_pointer_methods_and_embedded_aliases() {
    let mut engine = fixture_engine();
    let root = fixture_root();
    let worker = root.join("worker").join("extra.go");
    engine
        .open(
            worker,
            "package worker\nfunc Pair[A, B any](a A, b B) {}\nfunc (*Base) Tick() {}\n".into(),
            1,
        )
        .unwrap();
    let path = root.join("hierarchy_test.go");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(concat!(
        "\ntype LocalBase = worker.Base\n",
        "type ( TestOnly struct{ *LocalBase }; LocalReader = worker.Reader )\n",
        "type TestChild interface{ LocalReader }\ntype Indirect interface{ TestChild }\n",
        "type Twin interface{ Read() int }\ntype Count int\ntype Constraint interface{ Count }\n",
        "func len(i int) int { return i }\n",
        "func extra(b *worker.Base) {\n  defer b.Tick()\n  go (*worker.Base).Tick(b)\n",
        "  worker.Pair[int, string](1, \"\")\n  _ = Count(len(1))\n",
        "  { len := func(i int) int { return i }; _ = len(2) }\n}\n",
    ));
    engine.open(path.clone(), text.clone(), 1).unwrap();
    let extra = single(prepare_call_hierarchy(&mut engine, &path, at(&text, "extra(b")).unwrap());
    let outgoing = callees(&mut engine, &extra);
    assert_eq!(
        outgoing
            .iter()
            .map(|call| (call.to.name.as_str(), call.from_ranges.len()))
            .collect::<Vec<_>>(),
        [("len", 1), ("Pair", 1), ("(*Base).Tick", 2)]
    );
    let pair = single(prepare_call_hierarchy(&mut engine, &path, at(&text, "Pair[int")).unwrap());
    assert_eq!(outgoing[1].to, pair);
    assert_eq!(callers(&mut engine, &pair)[0].from, extra);
    let base = ty(&mut engine, "base-subtypes");
    let reader = ty(&mut engine, "reader-subtypes");
    assert_eq!(children(&mut engine, &base), ["TestOnly", "Derived"]);
    let test_only =
        single(prepare_type_hierarchy(&mut engine, &path, at(&text, "TestOnly")).unwrap());
    assert_eq!(test_only.range.start, test_only.selection_range.start);
    assert!(parents(&mut engine, &test_only).contains(&"LocalBase".to_owned()));
    let child = single(prepare_type_hierarchy(&mut engine, &path, at(&text, "TestChild")).unwrap());
    assert_eq!(parents(&mut engine, &child), ["LocalReader"]);
    let names = children(&mut engine, &reader);
    assert!(names.iter().any(|name| name == "TestChild"));
    assert!(
        !names
            .iter()
            .any(|name| matches!(name.as_str(), "Indirect" | "Twin" | "LocalReader"))
    );
    let constraint =
        single(prepare_type_hierarchy(&mut engine, &path, at(&text, "Constraint")).unwrap());
    assert!(parents(&mut engine, &constraint).is_empty());
}

#[test]
fn pending_loads_and_stale_searches_use_the_workspace_generation() {
    let root = fixture_root();
    let mut engine = Engine::new(root.clone());
    let leaf = call(&mut engine, "leaf-callers");
    let base = ty(&mut engine, "base-subtypes");
    let (path, pos) = case("prepare-cross-package-call");
    assert!(matches!(
        prepare_call_hierarchy(&mut engine, &path, pos).unwrap(),
        QueryProgress::Pending(_)
    ));
    let (path, pos) = case("prepare-alias-use");
    assert!(matches!(
        prepare_type_hierarchy(&mut engine, &path, pos).unwrap(),
        QueryProgress::Pending(_)
    ));
    let QueryProgress::Pending(load) = incoming_calls(&mut engine, &leaf).unwrap() else {
        panic!("expected test graph load")
    };
    assert!(load.include_tests);
    assert!(
        matches!(subtypes(&mut engine, &base).unwrap(), QueryProgress::Pending(required) if required == load)
    );
    assert!(engine.install_workspace(load, Workspace::load(&root, true).unwrap()));
    let QueryProgress::Searching(mut incoming) = incoming_calls(&mut engine, &leaf).unwrap() else {
        panic!("expected reference search")
    };
    let QueryProgress::Searching(mut subtypes) = subtypes(&mut engine, &base).unwrap() else {
        panic!("expected embedding search")
    };
    assert!(!incoming.advance(&mut engine).unwrap());
    engine.file_changed(&root.join("go.mod"), FileChangeType::CHANGED);
    assert!(incoming.advance(&mut engine).is_err());
    assert!(subtypes.advance(&mut engine).is_err());
}

#[test]
fn item_expansion_reads_the_current_declaration_and_rejects_a_replaced_target() {
    let project = TestProject::new();
    let text = "package sample\nfunc Alpha() { Beta() }\nfunc Beta() {}\ntype Outer struct { Inner }\ntype Inner struct{}\n";
    let path = project.write("sample.go", text);
    let mut engine = Engine::new(project.root.clone());
    assert!(engine.install_workspace(
        engine.workspace_load(),
        project.workspace(&["sample.go"], true)
    ));
    let alpha = single(prepare_call_hierarchy(&mut engine, &path, at(text, "Alpha")).unwrap());
    let outer = single(prepare_type_hierarchy(&mut engine, &path, at(text, "Outer")).unwrap());
    assert_eq!(callees(&mut engine, &alpha)[0].to.name, "Beta");
    let mut without_data = alpha.clone();
    without_data.data = None;
    assert_eq!(
        callees(&mut engine, &without_data),
        callees(&mut engine, &alpha)
    );
    engine
        .open(
            path.clone(),
            text.replace("Beta() }", "Alpha() }")
                .replace("{ Inner }", "{ F int }"),
            1,
        )
        .unwrap();
    let updated_alpha =
        single(prepare_call_hierarchy(&mut engine, &path, alpha.selection_range.start).unwrap());
    assert_eq!(callees(&mut engine, &alpha)[0].to, updated_alpha);
    assert_eq!(updated_alpha.range, span(1, 0, 24));
    let progress = supertypes(&mut engine, &outer).unwrap();
    assert!(complete(&mut engine, progress).unwrap().is_empty());
    engine.close(&path);
    assert_eq!(callees(&mut engine, &alpha)[0].to.name, "Beta");
    let progress = supertypes(&mut engine, &outer).unwrap();
    assert_eq!(complete(&mut engine, progress).unwrap()[0].name, "Inner");
    engine
        .open(
            path.clone(),
            text.replace("Alpha", "Other").replace("Outer", "OtherType"),
            2,
        )
        .unwrap();
    assert!(matches!(
        incoming_calls(&mut engine, &alpha).unwrap(),
        QueryProgress::Ready(None)
    ));
    assert!(matches!(
        outgoing_calls(&mut engine, &alpha).unwrap(),
        QueryProgress::Ready(None)
    ));
    assert!(matches!(
        subtypes(&mut engine, &outer).unwrap(),
        QueryProgress::Ready(None)
    ));
}
