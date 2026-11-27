use super::*;
use crate::test_support::TestProject;
use serde_json::Value;

fn run(text: &str, before: &str, new_name: &str) -> RenameResult {
    let project = TestProject::new();
    let path = project.write("sample.go", text);
    let mut engine = Engine::new(project.root.clone());
    engine.open(path.clone(), text.into(), 8).unwrap();
    engine.install_workspace(
        engine.workspace_load(),
        project.workspace(&["sample.go"], true),
    );
    let pos = engine
        .snapshot()
        .document(&path)
        .unwrap()
        .unwrap()
        .indexed()
        .lines
        .position(text, text.find(before).unwrap());
    match engine.rename(&path, pos, new_name).unwrap() {
        QueryProgress::Ready(result) => result,
        QueryProgress::Searching(mut task) => {
            while !task.advance(&mut engine).unwrap() {}
            task.finish()
        }
        QueryProgress::Pending(_) => panic!("graph installed"),
    }
}

#[test]
fn renames_all_uses_including_map_key_and_preserves_unrelated_names() {
    let edits = run(
        "package sample\nfunc f(k int) { _ = map[int]int{k: k}; { k := 2; _ = k } }\n",
        "k int",
        "计数",
    )
    .unwrap();
    let json = serde_json::to_value(edits).unwrap();
    let file = &json["documentChanges"][0];
    assert_eq!(file["textDocument"]["version"], 8);
    let edits = file["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 3);
    assert!(edits.iter().all(|e| e["newText"] == Value::from("计数")));
}

#[test]
fn rejects_binding_changes_in_either_direction_and_redeclarations() {
    for text in [
        "package sample\nfunc f(x int) { { y := 3; _ = x + y } }",
        "package sample\nvar y int\nfunc f(x int) { _ = x + y }",
        "package sample\nfunc f(x int, y int) {}",
    ] {
        assert!(
            matches!(run(text, "x int", "y"), Err(RenameError::Rejected(_))),
            "{text}"
        );
    }
}

#[test]
fn renames_type_and_implicit_field_together_only_if_both_bindings_survive() {
    let text = "package sample\ntype Inner int\ntype Outer struct { Inner }\nfunc read(v Outer) Inner { return Outer{Inner:v.Inner}.Inner }";
    let edits = run(text, "Inner int", "Number").unwrap();
    let json = serde_json::to_value(edits).unwrap();
    assert_eq!(
        json["documentChanges"][0]["edits"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    let duplicate = text.replace("struct { Inner }", "struct { Inner; Number int }");
    assert!(matches!(
        run(&duplicate, "Inner int", "Number"),
        Err(RenameError::Rejected(_))
    ));
}

#[test]
fn validates_complete_go_identifier_and_keeps_lexical_names_exact() {
    for name in ["变量٩", "_x", "𐐀", "nil", "len"] {
        assert!(valid_name(name), "{name}");
    }
    for name in ["", "_", "func", "٩x", "x\u{301}", "x²", "x y", "/*x*/"] {
        assert!(!valid_name(name), "{name}");
    }
}

#[test]
fn type_parameters_and_method_receivers_keep_independent_scopes() {
    let src = "package sample\ntype Box[T any] struct { Value T }\nfunc (recv Box[Elem]) Get() Elem { return recv.Value }\nfunc F[T any](arg T) T { return arg }";
    for (target, name, count) in [
        ("T any]", "Number", 2),
        ("recv Box", "receiver", 2),
        ("Elem]", "Item", 2),
        ("arg T", "v", 2),
    ] {
        let edits = serde_json::to_value(run(src, target, name).unwrap()).unwrap();
        assert_eq!(
            edits["documentChanges"][0]["edits"]
                .as_array()
                .unwrap()
                .len(),
            count,
            "{target}"
        );
    }
}

#[test]
fn refreshes_the_graph_for_an_unwatched_new_package_and_invalidates_in_flight_edits() {
    let project = TestProject::new();
    let file = project.write("a.go", "package sample\nconst Old = 1\n");
    let pos = Position::new(1, 6);
    let mut engine = Engine::new(project.root.clone());
    engine.install_workspace(engine.workspace_load(), project.workspace(&["a.go"], true));
    let QueryProgress::Searching(mut active) = engine.rename(&file, pos, "Updated").unwrap() else {
        panic!("expected a rename search")
    };
    project.write(
        "client/new.go",
        "package client\nimport p \"example.test/sample\"\nvar V = p.Old\n",
    );
    while !active.advance(&mut engine).unwrap() {}
    assert!(matches!(active.finish(), Err(RenameError::ContentModified)));
    assert!(matches!(
        engine.rename(&file, pos, "Updated").unwrap(),
        QueryProgress::Pending(_)
    ));
}
