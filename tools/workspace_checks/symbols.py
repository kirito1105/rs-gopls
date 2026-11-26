"""Check document and workspace symbols during edits and graph refreshes."""

from check_goland_symbols import DOCUMENT, WORKSPACE, request_document
from lsp_harness import (
    PausedGoList, collect_responses as symbol_responses, expect_error, notify_changes,
    session, site, wait_for_response, write,
)

from ._assertions import check_references, expect_outline, expect_workspace_symbols


def check_symbol_updates(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/symbols\n\ngo 1.22\n")
    original = "package symbols\nvar StableToken int\nvar OldToken int\nvar RemovedToken int\n"
    source = write(root / "symbols.go", original)
    edited = "// shift the original declaration\npackage symbols\nvar StableToken int\n\n"
    edited += 'var AddedToken = "🙂"; var RenamedToken int\n'
    old = tuple(site(source, f"var [[{name}]]", original)
                for name in ("StableToken", "OldToken", "RemovedToken"))
    new = tuple(site(source, f"var [[{name}]]", edited)
                for name in ("StableToken", "AddedToken", "RenamedToken"))

    def compare(client, targets, *extra):
        expect_outline(request_document(client, source), *((13, target, ()) for target in targets), *extra)
        expect_workspace_symbols(client.request(WORKSPACE, query="Token"), *targets)

    with session(executable_path, root, timeout, watch_files=True, hierarchical=True) as client:
        client.open_document(source, original)
        compare(client, old)
        client.change_document(source, 2, edited)
        compare(client, new)

        unfinished_function = edited + "func Broken() {\nvar LocalToken int\n"
        client.change_document(source, 3, unfinished_function)
        compare(client, new, (12, site(source, "func [[Broken]]", unfinished_function), ()))
        unfinished_struct = edited + "type Unclosed struct {\nField int\n"
        client.change_document(source, 4, unfinished_struct)
        compare(client, new)
        client.change_document(source, 5, edited)
        compare(client, new)

        disk = "// change on disk\n" + original.replace("OldToken", "DiskToken")
        write(source, disk)
        notify_changes(client, (source, 2))
        compare(client, new)
        client.close_document(source)
        restored = tuple(site(source, f"var [[{name}]]", disk)
                         for name in ("StableToken", "DiskToken", "RemovedToken"))
        compare(client, restored)

        draft = root / "draft.go"
        draft_text = "package symbols\nvar DraftToken int\n"
        draft_target = site(draft, "var [[DraftToken]]", draft_text)
        client.open_document(draft, draft_text)
        expect_outline(request_document(client, draft), (13, draft_target, ()))
        expect_workspace_symbols(client.request(WORKSPACE, query="Token"), *restored, draft_target)
        client.close_document(draft)
        expect_workspace_symbols(client.request(WORKSPACE, query="Token"), *restored)
        if draft.exists():
            raise RuntimeError(f"unsaved draft unexpectedly exists on disk: {draft}")

        watched = write(root / "watched.go", "package symbols\nvar WatchedToken int\n")
        notify_changes(client, (watched, 1))
        created = site(watched, "var [[WatchedToken]]")
        expect_outline(request_document(client, watched), (13, created, ()))
        expect_workspace_symbols(client.request(WORKSPACE, query="Token"), *restored, created)
        write(watched, "package symbols\n\nvar ChangedToken int\n")
        notify_changes(client, (watched, 2))
        changed = site(watched, "var [[ChangedToken]]")
        expect_outline(request_document(client, watched), (13, changed, ()))
        expect_workspace_symbols(client.request(WORKSPACE, query="Token"), *restored, changed)
        watched.unlink()
        notify_changes(client, (watched, 3))
        expect_workspace_symbols(client.request(WORKSPACE, query="Token"), *restored)


def workspace_symbol_request(request_id, query):
    return {"id": request_id, "method": WORKSPACE, "params": {"query": query}}


def check_no_extra_symbol_responses(client):
    try:
        message = client.get_next_response(timeout=0.2)
    except TimeoutError:
        return
    except RuntimeError as exc:
        raise RuntimeError(f"unexpected response after symbol requests completed: {exc}") from exc
    raise RuntimeError(f"unexpected response after symbol requests completed: {message!r}")


def check_symbols_waiting_for_graph(executable_path, root, timeout):
    gate = PausedGoList(root / "control", timeout)
    project = root / "project"
    module_text = "module example.test/symbols\n\ngo 1.22\n"
    module = write(project / "go.mod", module_text)
    disk = "package symbols\nvar BeforeToken int\n"
    path = write(project / "symbols.go", disk)
    latest = "// changed while the graph was loading\npackage symbols\nvar UpdatedToken int\n"
    updated = site(path, "var [[UpdatedToken]]", latest)

    with session(executable_path, project, timeout, watch_files=True, hierarchical=True, **gate.environment) as client:
        client.open_document(path, disk)
        expect_workspace_symbols(client.request(WORKSPACE, query="Token"), site(path, "var [[BeforeToken]]"))

        with gate.pause():
            write(module, module_text + "// refresh the test-inclusive package graph\n")
            notify_changes(client, (module, 2))
            client.send_batch(
                workspace_symbol_request("wait-for-symbol-graph", "Token"),
                workspace_symbol_request("cancel-symbol-graph", "Token"),
            )
            gate.wait_for_snapshot(min(timeout, 15))
            client.notify("$/cancelRequest", id="cancel-symbol-graph")
            expect_error(wait_for_response(client, "cancel-symbol-graph", min(timeout, 5)), -32800)
            client.change_document(path, 2, latest)
            client.send(id="outline-while-loading", method=DOCUMENT, params={"textDocument": {"uri": path.as_uri()}})
            outline = wait_for_response(client, "outline-while-loading", min(timeout, 5))
            expect_outline(outline.get("result"), (13, updated, ()))
            client.send(**workspace_symbol_request("also-waiting", "Token"))
            client.send(id="symbol-graph-barrier", method="rs-gopls/testUnknownMethod", params={})
            expect_error(wait_for_response(client, "symbol-graph-barrier", min(timeout, 5)), -32601)

        responses = symbol_responses(client, "wait-for-symbol-graph", "also-waiting")
        for response in responses.values():
            expect_workspace_symbols(response.get("result"), updated)
        client.send(**workspace_symbol_request("retry-symbol-graph", "Token"))
        expect_workspace_symbols(wait_for_response(client, "retry-symbol-graph").get("result"), updated)
        check_no_extra_symbol_responses(client)


def check_active_workspace_symbols(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/symbols\n\ngo 1.22\n")
    targets = []
    for number in range(512):
        name = f"Token{number:03d}"
        path = write(root / f"part{number:03d}.go", f"package symbols\nvar {name} int\n")
        targets.append(site(path, f"var [[{name}]]"))
    first = targets[0]
    latest = "// edited during a multi-batch search\n" + first.text
    shifted = site(first.path, "var [[Token000]]", latest)

    def cancel_one(client, stage):
        cancelled, waiting = f"cancel-{stage}", f"complete-{stage}"
        client.send_batch(
            workspace_symbol_request(cancelled, "Token"),
            workspace_symbol_request(waiting, "Token"),
            {"method": "$/cancelRequest", "params": {"id": cancelled}},
        )
        responses = symbol_responses(client, cancelled, waiting)
        expect_error(responses[cancelled], -32800)
        expect_workspace_symbols(responses[waiting].get("result"), *targets)

    with session(executable_path, root, timeout) as client:
        client.open_document(first.path, first.text)
        # References settle the test-inclusive package graph without building the symbol index.
        check_references(client, first, (first,), include_declaration=True)
        cancel_one(client, "indexing")
        cancel_one(client, "scanning")

        # The barrier and edit follow dispatch of the active query in the same stdio batch.
        client.send_batch(
            workspace_symbol_request("edited-during-symbol-scan", "Token"),
            {"id": "symbol-scan-barrier", "method": "rs-gopls/testUnknownMethod", "params": {}},
            {"method": "textDocument/didChange", "params": {
                "textDocument": {"uri": first.path.as_uri(), "version": 2},
                "contentChanges": [{"text": latest}],
            }},
        )
        expect_error(wait_for_response(client, "symbol-scan-barrier"), -32601)
        expect_error(wait_for_response(client, "edited-during-symbol-scan"), -32801)
        client.send_batch(
            workspace_symbol_request("retry-after-symbol-edit", "Token"),
            workspace_symbol_request("concurrent-after-symbol-edit", "Token"),
        )
        for response in symbol_responses(client, "retry-after-symbol-edit", "concurrent-after-symbol-edit").values():
            expect_workspace_symbols(response.get("result"), shifted, *targets[1:])
        check_no_extra_symbol_responses(client)


CASES = [
    ("symbol-updates", check_symbol_updates, 11),
    ("symbol-waiting-for-graph", check_symbols_waiting_for_graph, 12),
    ("symbol-active-search", check_active_workspace_symbols, 13),
]
