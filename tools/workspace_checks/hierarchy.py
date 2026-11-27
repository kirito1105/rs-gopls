"""Exercise workspace roots and cancellable call and type hierarchies over stdio."""

import time

from lsp_harness import PausedGoList, expect_error, next_message, path_from_uri, session, site, write
from ._assertions import check_location, expect_workspace_symbols
from .diagnostics import capability_session, folder


def check_hierarchy_folders(executable, root, timeout):
    base, first, extra = (root / name for name in ("base", "first", "extra"))
    write(base / "go.mod", "module example.test/base\n\ngo 1.22\n")
    base_file = write(base / "base.go", "package symbols\nconst FolderTokenBase = 1\n")
    base_token = site(base_file, "const [[FolderTokenBase]]")

    def consumer(path, name):
        write(path / "go.mod", f"module example.test/{name}\n\ngo 1.22\n"
              "require example.test/base v0.0.0\nreplace example.test/base => ../base\n")
        source = write(path / "consumer.go", 'package symbols\nimport base "example.test/base"\n'
                       f'const FolderToken{name.title()} = base.FolderTokenBase\n')
        return site(source, "base.[[FolderTokenBase]]"), site(source, f"const [[FolderToken{name.title()}]]")

    first_ref, first_token = consumer(first, "first")
    extra_ref, extra_token = consumer(extra, "extra")
    caps = {"textDocument": {"diagnostic": {}, "publishDiagnostics": {}},
            "workspace": {"workspaceFolders": True, "diagnostics": {"refreshSupport": True}}}
    with capability_session(executable, first, timeout, caps, folders=[first, base]) as (client, _):
        refresh_ids = set()

        def refresh():
            request = next_message(client, time.monotonic() + timeout)
            request_id = request.get("id")
            if (request.get("method") != "workspace/diagnostic/refresh"
                    or not isinstance(request_id, str) or request_id in refresh_ids):
                raise RuntimeError(f"expected a diagnostic refresh with a fresh ID, got {request!r}")
            refresh_ids.add(request_id)

        def symbols(*targets):
            expect_workspace_symbols(client.request("workspace/symbol", query="FolderToken"), *targets, kind=14)

        client.open_document(first_ref.path, first_ref.text)
        symbols(base_token, first_token)
        refresh()
        check_location(client, first_ref, base_token)
        check_location(client, extra_ref, None)
        for adding in (True, False):
            add, remove = ([folder(extra)], []) if adding else ([], [folder(extra)])
            client.notify("workspace/didChangeWorkspaceFolders", event={"added": add, "removed": remove})
            refresh()
            symbols(base_token, first_token, *([extra_token] if adding else []))
            refresh()
            check_location(client, extra_ref, base_token if adding else None)
            check_location(client, first_ref, base_token)
        try:
            push = client.get_notification(timeout=0)
        except TimeoutError:
            pass
        else:
            raise RuntimeError(f"client offering both diagnostic modes received {push!r}")


def check_call_type_hierarchy(executable, root, timeout):
    project = root / "project"
    write(project / "go.mod", "module example.test/hierarchy\n\ngo 1.22\n")
    worker = write(project / "worker" / "worker.go", "package worker\n"
                   "func Leaf() {}\nfunc Next() {}\n"
                   "type Contract interface{ Mark() }\ntype Base struct{}\nfunc (Base) Mark() {}\n"
                   "type Other struct{}\n")
    original = ('package sample\nimport "example.test/hierarchy/worker"\n'
                'type Derived struct{ worker.Base }\nfunc Run() { worker.Leaf(); worker.Leaf() }\n')
    caller = write(project / "caller.go", original)
    edited = original.replace("worker.Base", "worker.Other").replace("worker.Leaf", "worker.Next")
    gate = PausedGoList(root / "control", timeout)
    with session(executable, project, timeout, **gate.environment) as client:
        def prepare(kind, reference, target):
            result = client.request(f"textDocument/prepare{kind}Hierarchy",
                                    textDocument={"uri": reference.path.as_uri()}, position=reference.cursor())
            if (not result or len(result) != 1 or path_from_uri(result[0]["uri"]) != target.path
                    or result[0]["selectionRange"] != target.selection()
                    or result[0]["name"] != target.text[target.start:target.end]):
                raise RuntimeError(f"unexpected {kind} hierarchy target {result!r}")
            return result[0]

        def declaration(kind, target):
            return prepare(kind, target, target)

        client.open_document(caller, original)
        leaf = declaration("Call", site(worker, "func [[Leaf]]"))
        base = declaration("Type", site(worker, "type [[Base]]"))
        run = declaration("Call", site(caller, "func [[Run]]"))
        with gate.pause():
            client.send_batch(
                {"id": "cancel-incoming", "method": "callHierarchy/incomingCalls", "params": {"item": leaf}},
                {"id": "cancel-subtypes", "method": "typeHierarchy/subtypes", "params": {"item": base}},
                {"id": "stale-outgoing", "method": "callHierarchy/outgoingCalls", "params": {"item": run}},
            )
            gate.wait_for_snapshot()
            for request_id in ("cancel-incoming", "cancel-subtypes"):
                client.notify("$/cancelRequest", id=request_id)
                expect_error(client.wait_response(request_id), -32800)
            client.change_document(caller, 2, original + "\n")
            expect_error(client.wait_response("stale-outgoing"), -32801)

        def check_edges(text, callee, parent):
            call_target = site(worker, f"func [[{callee}]]")
            call_sites = [site(caller, f"worker.[[{callee}]](){suffix}", text) for suffix in (";", " }")]
            target = prepare("Call", call_sites[0], call_target)
            run = declaration("Call", site(caller, "func [[Run]]", text))
            ranges = [s.selection() for s in call_sites]
            outgoing = client.request("callHierarchy/outgoingCalls", item=run)
            incoming = client.request("callHierarchy/incomingCalls", item=target)
            if outgoing != [{"to": target, "fromRanges": ranges}] or incoming != [{"from": run, "fromRanges": ranges}]:
                raise RuntimeError(f"incorrect grouped call ranges: outgoing {outgoing!r}, incoming {incoming!r}")

            parent_item = prepare("Type", site(caller, f"worker.[[{parent}]]", text), site(worker, f"type [[{parent}]]"))
            derived = declaration("Type", site(caller, "type [[Derived]]", text))
            supers = client.request("typeHierarchy/supertypes", item=derived)
            subs = client.request("typeHierarchy/subtypes", item=parent_item)
            expected = [parent, "Contract"] if parent == "Base" else [parent]
            if sorted(item["name"] for item in supers) != sorted(expected) or parent_item not in supers or subs != [derived]:
                raise RuntimeError(f"incorrect {parent} hierarchy: supertypes {supers!r}, subtypes {subs!r}")

        check_edges(original, "Leaf", "Base")
        client.change_document(caller, 3, edited)
        check_edges(edited, "Next", "Other")
        if client.request("callHierarchy/incomingCalls", item=leaf) or client.request("typeHierarchy/subtypes", item=base):
            raise RuntimeError("unsaved edit retained a removed call or embedding")
        client.close_document(caller)
        check_edges(original, "Leaf", "Base")


CASES = [
    ("hierarchy-workspace-folders", check_hierarchy_folders),
    ("call-type-hierarchy", check_call_type_hierarchy),
]
