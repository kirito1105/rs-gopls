use std::fs;
use std::path::{Path, PathBuf};

use lsp_types::{Position, Range};
use serde_json::json;

use super::*;
use crate::semantic::Engine;
use crate::test_support::TestProject;
use crate::workspace::path_key;
use DiagnosticCode::*;

fn analyze_file(engine: &mut Engine, path: &Path) -> DiagnosticReport {
    let mut snapshot = engine.snapshot();
    let document = snapshot.document(path).unwrap().unwrap();
    analyze_document(&mut snapshot, &document).unwrap()
}

fn single_file(text: &str) -> DiagnosticReport {
    let project = TestProject::new();
    let path = project.write("sample.go", text);
    let mut engine = Engine::new(project.root.clone());
    assert!(engine.install_workspace(
        engine.workspace_load(),
        project.workspace(&["sample.go"], false)
    ));
    analyze_file(&mut engine, &path)
}

fn occurrence(text: &str, context: &str, spelling: &str) -> Range {
    let offset = text
        .find(context)
        .unwrap_or_else(|| panic!("missing context {context:?}"))
        + context.find(spelling).unwrap();
    let prefix = &text[..offset];
    let line = prefix.bytes().filter(|b| *b == b'\n').count() as u32;
    let col = prefix.rsplit('\n').next().unwrap().encode_utf16().count() as u32;
    Range::new(
        Position::new(line, col),
        Position::new(line, col + spelling.encode_utf16().count() as u32),
    )
}

fn item_at<'a>(
    report: &'a DiagnosticReport,
    text: &str,
    code: DiagnosticCode,
    context: &str,
    spelling: &str,
) -> &'a DiagnosticItem {
    let range = occurrence(text, context, spelling);
    report
        .items
        .iter()
        .find(|item| item.code == code && item.range == range)
        .unwrap_or_else(|| {
            panic!(
                "expected {code:?} at {range:?} in {context:?}, got {:#?}",
                report.items
            )
        })
}

#[test]
fn real_workspace_matches_every_diagnostics_fixture() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/fixtures/diagnostics");
    let graph = Workspace::load(&root, true).unwrap();
    let mut engine = Engine::new(root.clone());
    assert!(engine.install_workspace(engine.workspace_load(), graph));
    type Expected = (DiagnosticCode, &'static str, &'static str);
    let cases: &[(&str, &[Expected])] = &[
        ("clean/clean.go", &[]),
        ("clean/clean_test.go", &[]),
        (
            "broken_syntax/missing_paren.go",
            &[(SyntaxError, "r\"", "\"")],
        ),
        (
            "broken_syntax/missing_expression.go",
            &[(SyntaxError, "+ )", ")")],
        ),
        (
            "semantic_errors/imports.go",
            &[(ImportUnused, "\"strings\"", "\"strings\"")],
        ),
        (
            "semantic_errors/invalid_import/invalid.go",
            &[(
                ImportInvalid,
                "\"example.test/diagnostics/nonexistent\"",
                "\"example.test/diagnostics/nonexistent\"",
            )],
        ),
        (
            "semantic_errors/self_import/self.go",
            &[(
                ImportSelf,
                "\"example.test/diagnostics/semantic_errors/self_import\"",
                "\"example.test/diagnostics/semantic_errors/self_import\"",
            )],
        ),
        (
            "semantic_errors/names.go",
            &[
                (NameUnresolved, "return missingValue", "missingValue"),
                (TypeUnresolved, "UnresolvedType MissingType", "MissingType"),
                (
                    FieldOrMethodUnresolved,
                    "record.MissingField",
                    "MissingField",
                ),
                (
                    FieldOrMethodUnresolved,
                    "record.MissingMethod",
                    "MissingMethod",
                ),
            ],
        ),
        (
            "semantic_errors/declarations.go",
            &[
                (NameRedeclared, "var Repeated", "Repeated"),
                (VarUnused, "unused := 1", "unused"),
                (ShortVarNoNewVars, "value := value", ":="),
                (ConstReassign, "Fixed = 2", "Fixed"),
            ],
        ),
        (
            "semantic_errors/calls.go",
            &[
                (CallArity, "return Add(1)", "(1)"),
                (ReturnArity, "return 1", "return 1"),
                (CallNonFunction, "value()", "value"),
            ],
        ),
        (
            "semantic_errors/types.go",
            &[
                (TypeMismatch, "= \"text\"", "\"text\""),
                (InterfaceNotImplemented, "= Item{}", "Item{}"),
                (CompositeLiteralUnknownField, "Item{Missing", "Missing"),
                (DuplicateSwitchCase, "case 1:\n\t\treturn 2", "1"),
                (BlankAsValue, "BlankValue = _", "_"),
            ],
        ),
    ];
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("diagnostics-queries.json")).unwrap())
            .unwrap();
    assert_eq!(cases.len(), manifest["cases"].as_array().unwrap().len());
    for (file, expected) in cases {
        assert!(
            manifest["cases"]
                .as_array()
                .unwrap()
                .iter()
                .any(|case| case["path"] == *file)
        );
        let path = root.join(file);
        let text = fs::read_to_string(&path).unwrap();
        let report = analyze_file(&mut engine, &path);
        assert!(!report.is_partial, "{file}");
        if expected.is_empty() {
            assert!(report.items.is_empty(), "{file}: {:#?}", report.items);
        }
        assert_eq!(
            report
                .items
                .iter()
                .filter(|item| item.code != PackageLoad)
                .count(),
            expected.len(),
            "{file}: {:#?}",
            report.items
        );
        for (code, context, spelling) in *expected {
            let context = context.replace('\n', if text.contains("\r\n") { "\r\n" } else { "\n" });
            let item = item_at(&report, &text, *code, &context, spelling);
            match code {
                ImportUnused => {
                    assert_eq!(item.tags, [DiagnosticTag::Unnecessary]);
                    assert_eq!(
                        item.fix_data,
                        DiagnosticFixData::RemoveUnusedImport {
                            import_path: "strings".into(),
                            spec_range: item.range
                        }
                    );
                }
                VarUnused => {
                    assert_eq!(item.tags, [DiagnosticTag::Unnecessary]);
                    assert_eq!(
                        item.fix_data,
                        DiagnosticFixData::ReplaceUnusedVarWithBlank {
                            name: "unused".into(),
                            ident_range: item.range
                        }
                    );
                }
                ShortVarNoNewVars => assert_eq!(
                    item.fix_data,
                    DiagnosticFixData::ChangeShortDeclToAssign {
                        operator_range: item.range
                    }
                ),
                _ => {}
            }
        }
    }
}

#[test]
fn partial_reports_upgrade_and_cache_checks_all_snapshot_fields() {
    let project = TestProject::new();
    let text = "package sample\nvar X = missing\n";
    let path = project.write("sample.go", text);
    let mut engine = Engine::new(project.root.clone());
    engine.open(path.clone(), text.into(), 4).unwrap();
    let mut cache = DiagnosticsCache::default();
    let partial = {
        let mut s = engine.snapshot();
        let d = s.document(&path).unwrap().unwrap();
        let report = get_or_compute(&mut cache, &mut s, &d).unwrap();
        assert!(report.is_partial && report.items.is_empty());
        report.result_id.clone()
    };
    engine.install_workspace(
        engine.workspace_load(),
        project.workspace(&["sample.go"], false),
    );
    {
        let mut s = engine.snapshot();
        let d = s.document(&path).unwrap().unwrap();
        let report = get_or_compute(&mut cache, &mut s, &d).unwrap();
        assert!(!report.is_partial);
        assert_ne!(partial, report.result_id);
        item_at(report, text, NameUnresolved, "missing", "missing");
        let address = std::ptr::from_ref(report);
        assert_eq!(
            address,
            std::ptr::from_ref(get_or_compute(&mut cache, &mut s, &d).unwrap())
        );
        assert!(cache.get(&d, 0, false).is_some());
        assert!(cache.get(&d, 0, true).is_none());
        assert!(cache.get(&d, 1, false).is_none());
    }
    for (version, content) in [(5, text), (4, "package sample\nvar X = 1\n")] {
        engine.open(path.clone(), content.into(), version).unwrap();
        assert!(
            cache
                .get(
                    &engine.snapshot().document(&path).unwrap().unwrap(),
                    0,
                    false
                )
                .is_none()
        );
    }
    engine.open(path.clone(), text.into(), 4).unwrap();
    let mut s = engine.snapshot();
    let d = s.document(&path).unwrap().unwrap();
    let report = cache.get(&d, 0, false).unwrap().clone();
    cache.invalidate_document(&path_key(&path));
    assert!(cache.get(&d, 0, false).is_none());
    cache.insert(report.clone());
    for i in 0..256 {
        let mut another = report.clone();
        another.snapshot.path_key = PathBuf::from(format!("doc{i}.go"));
        cache.insert(another);
    }
    assert!(cache.get(&d, 0, false).is_none());
    cache.insert(report);
    cache.clear();
    assert!(cache.get(&d, 0, false).is_none());
}

#[test]
fn package_errors_follow_go_byte_columns_and_unsaved_syntax_repairs() {
    let project = TestProject::new();
    let text = "package sample\r\nvar X = \"🙂\" + )\r\n";
    let path = project.write("sample.go", text);
    project.write("other.go", "package sample\n");
    let column = text.lines().nth(1).unwrap().find(')').unwrap() + 1;
    let listing = json!({"Dir":project.root,"ImportPath":"sample","Name":"sample","GoFiles":["sample.go","other.go"],
        "Error":{"Err":"expected operand, found ')'","Pos":format!("sample.go:2:{column}")},
        "DepsErrors":[{"Err":"dependency metadata unavailable"},{"Err":"unrelated file","Pos":"other.go:1:1"}]});
    let graph = Workspace::from_test_listing(
        std::slice::from_ref(&project.root),
        &listing.to_string(),
        false,
    );
    let mut engine = Engine::new(project.root.clone());
    engine.install_workspace(engine.workspace_load(), graph);
    let report = analyze_file(&mut engine, &path);
    item_at(&report, text, PackageLoad, "+ )", ")");
    item_at(
        &report,
        text,
        PackageLoad,
        "package sample",
        "package sample",
    );
    assert!(!report.items.iter().any(|i| i.message == "unrelated file"));
    assert!(
        report
            .items
            .iter()
            .any(|i| i.code == SyntaxError && i.range.start < i.range.end)
    );
    engine
        .open(path.clone(), text.replace("+ )", ""), 2)
        .unwrap();
    let repaired = analyze_file(&mut engine, &path);
    assert_eq!(repaired.items.len(), 1, "{:?}", repaired.items);
    assert_eq!(repaired.items[0].message, "dependency metadata unavailable");
}

#[test]
fn unresolved_qualifier_after_astral_character_carries_exact_import_fix() {
    let text = "package sample\r\nfunc f(){ _ = \"🙂\"; ghost.Use() }\r\n";
    let report = single_file(text);
    let item = item_at(&report, text, NameUnresolved, "ghost.Use()", "ghost");
    assert_eq!(
        item.fix_data,
        DiagnosticFixData::ImportMissingPackage {
            qualifier: "ghost".into(),
            ident_range: item.range
        }
    );
}

#[test]
fn valid_scopes_calls_promotions_and_assignability_stay_clean() {
    let text = r#"package sample
type N int64
type Label struct { Text string }
func (Label) Name(x int) string { return "" }
type Box struct { Label }
type Reader interface { Name(int) string }
type Nested interface { Reader }
type Callback func(int) string
func pair() (int, string) { return 1, "ok" }
func take(x int, values ...string) {}
func forward() (int, string) { return pair() }
func named() (a, b int) { return }
func generic[T any](x T) T { return x }
func test(c chan int, v any, cb Callback, n N, box Box, args ...string) {
    var _ Nested = box
    take(pair())
    take(1, args...)
    take(2)
    _ = cb(1)
    _ = Box.Name(box, 1)
    _ = (*Box).Name(&box, 1)
    _ = box.Text
    _ = Box{Label: Label{Text:"text"}}
    _ = []byte("hi")
    _ = int64(n)
    var count N = 1
    count = 100
    _ = count
    _ = generic[int](1)
    a := 1
    a, b := a, 2
    if b := b; b != 0 { f := func(){ _ = a }; f() }
    if x := true; x { _ = x } else if !x {}
    for i := 0; i < b; i++ { b := i; _ = b }
    for _, word := range args { _ = word }
    select { case x := <-c: x, y := x, 2; _ = x+y; default: }
    switch t := v.(type) { case int: _ = t; case string: _ = t; default: }
    key := 1; _ = map[int]string{key: "value"}
    _ = make([]int, 1, 2)
    const zero = 0
    var _ N = zero
}
"#;
    let report = single_file(text);
    assert!(report.items.is_empty(), "{:#?}", report.items);
}

#[test]
fn checks_arity_types_and_builtin_shadowing_only_when_determined() {
    let text = r#"package sample
type Count int
type Base struct{}
func (*Base) Mark(int) string { return "" }
type Marker interface { Mark(int) string }
type Wrong struct{}
func (Wrong) Mark(string) string { return "" }
func two() (int,int) { return 1,2 }
func one(n int) {}
func typed() int { return "bad" }
func check(b Base, n int, s string, args []string) {
    var p = nil
    q := nil
    _, _ = p, q
    if n {}
    for ; "bad"; {}
    one(two())
    one("argument")
    n = s
    var _ Marker = b
    var _ Marker = &b
    var _ Marker = Wrong{}
    _ = (*Base).Mark(&b)
    _ = len()
    _ = make([]int)
    _ = append()
    _ = copy(args)
    delete()
    close()
    _ = new()
    len := func() int { return 1 }
    _ = len()
}
"#;
    let report = single_file(text);
    for (code, context, spelling) in [
        (NilWithoutType, "p = nil", "nil"),
        (NilWithoutType, "q := nil", "nil"),
        (ConditionNotBool, "if n", "n"),
        (ConditionNotBool, "; \"bad\";", "\"bad\""),
        (CallArity, "one(two())", "(two())"),
        (TypeMismatch, "one(\"argument\")", "\"argument\""),
        (TypeMismatch, "n = s", "s"),
        (TypeMismatch, "return \"bad\"", "\"bad\""),
        (InterfaceNotImplemented, "Marker = b", "b"),
        (InterfaceNotImplemented, "Marker = Wrong{}", "Wrong{}"),
        (CallArity, ".Mark(&b)", "(&b)"),
        (CallArity, "= len()", "()"),
        (CallArity, "make([]int)", "([]int)"),
        (CallArity, "= append()", "()"),
        (CallArity, "copy(args)", "(args)"),
        (CallArity, "delete()", "()"),
        (CallArity, "close()", "()"),
        (CallArity, "new()", "()"),
    ] {
        item_at(&report, text, code, context, spelling);
    }
    assert_eq!(report.items.len(), 18, "{:#?}", report.items);
}

#[test]
fn unused_reads_and_redeclarations_respect_go_blocks() {
    let text = r#"package sample
type S struct { X, X int; Field int }
func (S) Field() {}
type I interface { A(); A() }
func dup[T, T any](x, x int) {}
func f(v any) {
    var a int
    a = 1
    b := 1
    b++
    const c = 1
    c += 2
    var c = 2
    switch r := v.(type) { case int: default: }
    d := 0
    { d := 1; _ = d }
    _ = func() int { return d }
    var x int
    var x string
    _, x := 1, 2
    _ = x
}
func init() {}
func init() {}
"#;
    let report = single_file(text);
    for (code, context, spelling) in [
        (NameRedeclared, ", X int", "X"),
        (NameRedeclared, ") Field()", "Field"),
        (NameRedeclared, "; A()", "A"),
        (NameRedeclared, ", T any", "T"),
        (NameRedeclared, ", x int", "x"),
        (VarUnused, "a int\n    a =", "a"),
        (ConstReassign, "c +=", "c"),
        (NameRedeclared, "var c = 2", "c"),
        (VarUnused, "r := v", "r"),
        (NameRedeclared, "var x string", "x"),
        (ShortVarNoNewVars, "_, x :=", ":="),
    ] {
        item_at(&report, text, code, context, spelling);
    }
    assert_eq!(report.items.len(), 11, "{:#?}", report.items);
}

#[test]
fn graph_edges_package_names_and_private_members() {
    let project = TestProject::new();
    let text = r#"package sample
import "p/v2"
import again "q"
import again "other"
import . "dot"
import _ "cycle"
type Wrapper struct { named.Public }
var _ = named.secret
var _ = named.Public{}.hidden
var _ = named.Public{}.Visible
var _ = Wrapper{}.Visible
var _ = FromDot
func f() { var _ External; const n = 1; _ = n }
"#;
    let path = project.write("sample.go", text);
    let mut listing = vec![
        json!({"Dir":project.root,"ImportPath":"sample","Name":"sample","GoFiles":["sample.go"],"Imports":["p/v2","q","other","dot","cycle"]}),
    ];
    for (dir, id, name, source, imports) in [
        (
            "p",
            "p/v2",
            "named",
            "const secret = 1\ntype Public struct { hidden, Visible int }",
            vec![],
        ),
        ("q", "q", "q", "", vec![]),
        ("other", "other", "other", "", vec![]),
        (
            "dot",
            "dot",
            "dot",
            "var FromDot = 1\ntype External int",
            vec![],
        ),
        (
            "cycle",
            "cycle",
            "cycle",
            "import _ \"bridge\"",
            vec!["bridge"],
        ),
        (
            "bridge",
            "bridge",
            "bridge",
            "import _ \"sample\"",
            vec!["sample"],
        ),
    ] {
        project.write(
            &format!("{dir}/file.go"),
            &format!("package {name}\n{source}\n"),
        );
        listing.push(json!({"Dir":project.root.join(dir),"ImportPath":id,"Name":name,"GoFiles":["file.go"],"Imports":imports}));
    }
    let graph = Workspace::from_test_listing(
        std::slice::from_ref(&project.root),
        &listing
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        false,
    );
    let mut engine = Engine::new(project.root.clone());
    engine.install_workspace(engine.workspace_load(), graph);
    let report = analyze_file(&mut engine, &path);
    for (code, context, spelling) in [
        (ImportCycle, "\"cycle\"", "\"cycle\""),
        (ImportDuplicateAlias, "again \"other\"", "again"),
        (ImportUnused, "again \"q\"", "again"),
        (NameUnexported, "named.secret", "secret"),
        (NameUnexported, "Public{}.hidden", "hidden"),
    ] {
        item_at(&report, text, code, context, spelling);
    }
    assert_eq!(report.items.len(), 6, "{:#?}", report.items);
}

#[test]
fn clean_standard_library_sources_from_the_installed_graph() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/fixtures/diagnostics");
    let graph = Workspace::load(&root, true).unwrap();
    let mut files = Vec::new();
    for package in [
        "errors", "io", "strings", "fmt", "bytes", "bufio", "strconv", "slices", "reflect", "sync",
    ] {
        let package = graph.package(package).unwrap();
        files.extend(package.files.iter().cloned());
    }
    let mut engine = Engine::new(root);
    engine.install_workspace(engine.workspace_load(), graph);
    let mut errors = Vec::new();
    for file in files {
        let report = analyze_file(&mut engine, &file);
        for item in report.items {
            errors.push((
                file.file_name().unwrap().to_owned(),
                item.range.start,
                item.code,
                item.message,
            ));
        }
    }
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn multi_value_types_and_variadic_element_types() {
    let text = r#"package sample
func pair() (string, int) { return "", 1 }
func want(a int, b int) {}
func forward() (string,string) { return pair() }
func call() {
    want(pair())
    var a, b int = pair()
    _, _ = a, b
}
func first(xs ...[]int) []int { return xs[0] }
type BadError struct{}
func (BadError) Error() int { return 0 }
var _ error = BadError{}
var number = 7
func bad(){ number() }
"#;
    let report = single_file(text);
    for (code, context, spelling) in [
        (TypeMismatch, "return pair()", "pair()"),
        (TypeMismatch, "want(pair())", "pair()"),
        (TypeMismatch, "int = pair()", "pair()"),
        (InterfaceNotImplemented, "error = BadError{}", "BadError{}"),
        (CallNonFunction, "number()", "number"),
    ] {
        item_at(&report, text, code, context, spelling);
    }
    assert_eq!(report.items.len(), 5, "{:#?}", report.items);
}

#[test]
fn syntax_recovery_skips_the_damaged_function_and_checks_intact_siblings() {
    let text = "package sample\r\nfunc bad(){ _ = \"🙂\" + ) }\r\nfunc good() { _ = missing }\r\n";
    let project = TestProject::new();
    let path = project.write("sample.go", text);
    let mut engine = Engine::new(project.root.clone());
    let partial = analyze_file(&mut engine, &path);
    assert!(partial.is_partial);
    assert_eq!(partial.items.len(), 1, "{:#?}", partial.items);
    item_at(&partial, text, SyntaxError, "+ )", ")");
    engine.install_workspace(
        engine.workspace_load(),
        project.workspace(&["sample.go"], false),
    );
    let report = analyze_file(&mut engine, &path);
    item_at(&report, text, NameUnresolved, "= missing", "missing");
    assert_eq!(report.items.len(), 2, "{:#?}", report.items);
}

#[test]
fn package_declarations_and_new_imports_use_open_buffers() {
    let project = TestProject::new();
    let sibling = project.write("a.go", "package sample\nvar Defined int\n");
    let text =
        "package sample\nvar Defined = 1\nvar _ = Overlay\nfunc f(){ var x Defined; _ = x }\n";
    let path = project.write("b.go", text);
    let mut engine = Engine::new(project.root.clone());
    engine.install_workspace(
        engine.workspace_load(),
        project.workspace(&["a.go", "b.go"], false),
    );
    let report = analyze_file(&mut engine, &path);
    item_at(&report, text, NameRedeclared, "var Defined", "Defined");
    item_at(&report, text, TypeUnresolved, "x Defined", "Defined");
    engine
        .open(sibling, "package sample\nvar Overlay = 2\n".into(), 2)
        .unwrap();
    engine.open(path.clone(),"package sample\nimport \"math/bits\"\nimport \"C\"\nvar _ = bits.Len(Overlay)\nvar _ C.int\n".into(),3).unwrap();
    let report = analyze_file(&mut engine, &path);
    assert!(report.items.is_empty(), "{:#?}", report.items);
}

#[test]
fn repaired_go_list_parse_error_disappears_before_the_file_is_saved() {
    let project = TestProject::new();
    project.write("go.mod", "module example.test/parse\n\ngo 1.24\n");
    let path = project.write("sample.go", "package 123\n");
    let graph = Workspace::load(&project.root, false).unwrap();
    let mut engine = Engine::new(project.root.clone());
    engine.install_workspace(engine.workspace_load(), graph);
    let disk = analyze_file(&mut engine, &path);
    assert!(
        disk.items.iter().any(|item| item.code == PackageLoad),
        "{:?}",
        disk.items
    );
    engine
        .open(path.clone(), "package sample\n".into(), 1)
        .unwrap();
    let fixed = analyze_file(&mut engine, &path);
    assert!(fixed.items.is_empty(), "{:?}", fixed.items);
}

#[test]
fn escaped_imports_do_not_suggest_importing_an_existing_qualifier() {
    let text = r#"package sample
import "ma\x74h"
var _ = math.Pi
"#;
    assert!(single_file(text).items.is_empty());
}
