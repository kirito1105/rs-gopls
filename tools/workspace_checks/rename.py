"""Check versioned rename edits, snapshot invalidation, and the shared GoLand fixtures."""

from functools import partial
import shutil

from fixtures.rename import replay
from lsp_harness import PausedGoList, expect_error, session, site, write
from ._assertions import check_references


RENAME = "textDocument/rename"
PREPARE = "textDocument/prepareRename"


def params(at):
    return {"textDocument": {"uri": at.path.as_uri()}, "position": at.cursor()}


def request_error(client, method, code, **arguments):
    response = client.wait_response(client.start_request(method, **arguments))
    expect_error(response, code)
    if "result" in response or not response["error"].get("message"):
        raise RuntimeError(f"expected a reason with no partial edit: {response}")


def apply_edit(root, opened, edit):
    try:
        replay.apply_workspace_edit(root, opened, edit)
    except (KeyError, ValueError) as exc:
        raise RuntimeError(f"invalid workspace edit: {exc}") from exc


def check_capabilities(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/renamecaps\n\ngo 1.22\n")
    text = ('package sample\nvar target int\nvar _ = len("unchanged")\n'
            'func shadow(len int) int { return len }\n')
    source = write(root / "app.go", text)
    at = params(site(source, "var [[target]]"))
    with session(executable_path, root, timeout, prepare_rename=True) as client:
        request_error(client, RENAME, -32601, **at, newName="updated")
        request_error(client, PREPARE, -32601, **at)
    with session(executable_path, root, timeout, document_changes=True) as client:
        request_error(client, PREPARE, -32601, **at)
        for invalid in ["_", "func", "two words", "3value", ""]:
            request_error(client, RENAME, -32803, **at, newName=invalid)
        unchanged = client.request(RENAME, **at, newName="target")
        if unchanged != {"documentChanges": []}:
            raise RuntimeError(f"renaming to the same name must be an empty edit: {unchanged}")
    with session(executable_path, root, timeout, document_changes=True, prepare_rename=True) as client:
        for anchor in ['var [[_]] =', '= [[len]](', 'len("[[unchanged]]")', 'package [[sample]]']:
            position = params(site(source, anchor))
            if client.request(PREPARE, **position) is not None:
                raise RuntimeError(f"unexpectedly prepared an unrenamable target: {anchor}")
            request_error(client, RENAME, -32803, **position, newName="Updated")
        shadow = site(source, "return [[len]]")
        prepared = client.request(PREPARE, **params(shadow))
        if prepared != {"range": shadow.selection(), "placeholder": "len"}:
            raise RuntimeError(f"shadowing parameter must be selected by declaration identity: {prepared}")
        edit = client.request(RENAME, **params(shadow), newName="长度")
    apply_edit(root, {}, edit)
    expected = text.replace("shadow(len int) int { return len }", "shadow(长度 int) int { return 长度 }")
    if source.read_bytes() != expected.encode("utf-8"):
        raise RuntimeError("renaming the shadowing parameter changed other occurrences of len")
    replay.go_test(root, timeout)


def check_overlays(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/renamebuffers\n\ngo 1.22\n")
    decl = write(root / "decl.go", "package sample\r\nconst Target = 5\r\n")
    use = write(root / "use.go", "package sample\r\nvar FromDisk = 3\r\n")
    draft = root / "draft.go"
    use_text = "package sample\r\nvar FromDisk = /* 🧪 */ Target\r\n"
    draft_text = "package sample\r\nvar Draft = Target\r\n"
    opened = {use: (7, use_text), draft: (1, draft_text)}
    at = site(use, "/* 🧪 */ [[Target]]", use_text)
    original = decl.read_bytes()
    with session(executable_path, root, timeout, document_changes=True, prepare_rename=True) as client:
        client.open_document(use, use.read_bytes().decode("utf-8"))
        client.change_document(use, 7, use_text)
        client.open_document(draft, draft_text)
        prepare = client.request(PREPARE, **params(at))
        if prepare != {"range": at.selection(), "placeholder": "Target"}:
            raise RuntimeError(f"incorrect UTF-16 selection after an unsaved edit: {prepare}")
        comment = site(use, "/* [[🧪]] */", use_text)
        if client.request(PREPARE, **params(comment)) is not None:
            raise RuntimeError("prepareRename selected a comment")
        request_error(client, RENAME, -32803, **params(comment), newName="Other")
        edit = client.request(RENAME, **params(at), newName="Updated值")
        if decl.read_bytes() != original or draft.exists():
            raise RuntimeError("server changed workspace files before the client applied its edit")
    apply_edit(root, opened, edit)
    for path, text in [(decl, original.decode("utf-8")), (use, use_text), (draft, draft_text)]:
        if path.read_bytes() != text.replace("Target", "Updated值").encode("utf-8"):
            raise RuntimeError(f"applied changes differ from the expected CRLF document: {path}")
    replay.go_test(root, timeout)


def check_cancellation(executable_path, root, timeout):
    gate = PausedGoList(root / "control", timeout)
    project = root / "project"
    write(project / "go.mod", "module example.test/renamecancel\n\ngo 1.22\n")
    path = write(project / "decl.go", "package sample\nvar Target int\n")
    at = params(site(path, "var [[Target]]"))
    source = "package sample\nvar Use = Target\n"
    other = write(project / "other.go", source)
    for index in range(64):
        write(project / f"use_{index}.go", f"package sample\nvar V{index} = Target\n")
    args = {**at, "newName": "Renamed"}
    with session(executable_path, project, timeout, document_changes=True,
                 watch_files=True, **gate.environment) as client:
        with gate.pause():
            cancelled = client.start_request(RENAME, **args)
            gate.wait_for_snapshot()
            client.notify("$/cancelRequest", id=cancelled)
            expect_error(client.wait_response(cancelled), -32800)
            stale = client.start_request(RENAME, **args)
            client.open_document(other, source)
            expect_error(client.wait_response(stale), -32801)
        # Complete one request so the following requests run against a ready graph.
        client.request(RENAME, **args)
        changed_use = project / "use_0.go"
        for identity, event, code in [
            ("cancel-active", {"method": "$/cancelRequest", "params": {"id": "cancel-active"}}, -32800),
            ("disk-active", {"method": "workspace/didChangeWatchedFiles", "params": {
                "changes": [{"uri": changed_use.as_uri(), "type": 2}],
            }}, -32801),
        ]:
            if identity == "disk-active":
                write(changed_use, "// changed on disk\npackage sample\nvar V0 = Target\n")
            client.send_batch({"id": identity, "method": RENAME, "params": args}, event)
            expect_error(client.wait_response(identity), code)
        current = client.request(RENAME, **args)
        apply_edit(project, {other: (1, source)}, current)
    if changed_use.read_text(encoding="utf-8") != "// changed on disk\npackage sample\nvar V0 = Renamed\n":
        raise RuntimeError("retry did not rename the usage at its latest disk position")
    replay.go_test(project, timeout)


def check_case_bindings(executable_path, root, timeout):
    body = "ptr := &value; value, n := value+1, 2; return *ptr+n+0*value"
    for kind, signature, guard, tail, call in [
        ("switch", "(arg any)", "switch value := arg.(type) { case int:", "}; return 0", "Calc(4)"),
        ("select", "()", "c := make(chan int, 1); c <- 4; select { case value := <-c:", "}", "Calc()"),
    ]:
        project = root / kind
        write(project / "go.mod", "module example.test/casebinding\n\ngo 1.22\n")
        text = f"package sample\nfunc Calc{signature} int {{ {guard} {body} {tail} }}\n"
        path = write(project / "calc.go", text)
        write(project / "calc_test.go", 'package sample\nimport "testing"\n'
              f'func TestCalc(t *testing.T) {{ if got := {call}; got != 7 {{ t.Fatal(got) }} }}\n')
        at = site(path, "[[value]] :=")
        uses = [site(path, anchor) for anchor in
                ["&[[value]]", "[[value]], n :=", ":= [[value]]+1", "0*[[value]]"]]
        replay.go_test(project, timeout)
        with session(executable_path, project, timeout, document_changes=True, prepare_rename=True) as client:
            check_references(client, at, (*uses, at), include_declaration=True)
            prepare = client.request(PREPARE, **params(at))
            if prepare != {"range": at.selection(), "placeholder": "value"}:
                raise RuntimeError(f"{kind}: could not prepare the case binding: {prepare}")
            edit = client.request(RENAME, **params(at), newName="item")
        apply_edit(project, {}, edit)
        if path.read_bytes() != text.replace("value", "item").encode("utf-8"):
            raise RuntimeError(f"{kind}: the same-block short declaration did not reuse the case variable")
        replay.go_test(project, timeout)


def check_unwatched_new_importer(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/refresh\n\ngo 1.22\n")
    path = write(root / "decl.go", "package sample\nconst Old = 4\n")
    at = params(site(path, "const [[Old]]"))
    with session(executable_path, root, timeout, document_changes=True, prepare_rename=True) as client:
        client.request(PREPARE, **at)
        usage = write(root / "client" / "new.go", 'package client\nimport p "example.test/refresh"\nvar V = p.Old\n')
        edit = client.request(RENAME, **at, newName="Updated")
    replay.apply_workspace_edit(root, {}, edit)
    if "p.Updated" not in usage.read_text(encoding="utf-8"):
        raise RuntimeError("rename missed the new importer outside the cached graph")
    replay.go_test(root, timeout)


def check_fixture(case, executable_path, root, timeout):
    project = shutil.copytree(replay.SOURCE, root)
    try:
        replay.replay(executable_path, project, case, timeout)
    except (KeyError, ValueError) as exc:
        raise RuntimeError(f"{case['id']}: {exc}") from exc


CASES = [
    ("rename-capabilities", check_capabilities),
    ("rename-overlays", check_overlays),
    ("rename-cancellation", check_cancellation),
    ("rename-unwatched-importer", check_unwatched_new_importer),
    ("rename-case-bindings", check_case_bindings),
    *((f"rename-{name}", partial(check_fixture, case)) for name, case in replay.CASES.items()),
]
