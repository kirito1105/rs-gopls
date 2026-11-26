"""Check hover and signature help across overlays and graph refreshes."""

from lsp_harness import PausedGoList, notify_changes, session, site, wait_for_response, write


def editor_help_params(query):
    return {"textDocument": {"uri": query.path.as_uri()}, "position": query.cursor()}


def expect_editor_hover(result, query, signature, doc=""):
    contents = f"```go\n{signature}\n```"
    if doc:
        contents += f"\n\n{doc}"
    expected = {"contents": {"kind": "markdown", "value": contents}, "range": query.selection()}
    if result != expected:
        raise RuntimeError(f"hover expected {expected!r}, received {result!r}")


def expect_editor_signature(result, label, doc="", active=0):
    try:
        [signature] = result["signatures"]
        markdown = {"kind": "markdown", "value": doc} if doc else None
        if (result.get("activeSignature"), result.get("activeParameter")) == (0, active) and (
            signature["label"], signature.get("documentation")
        ) == (label, markdown):
            return
    except (KeyError, TypeError, ValueError):
        pass
    raise RuntimeError(f"expected signature {label!r} with active parameter {active}, received {result!r}")


def check_editor_help_overlays(executable_path, root, timeout):
    signature_method = "textDocument/signatureHelp"
    hover_method = "textDocument/hover"
    write(root / "go.mod", "module example.test/help\n\ngo 1.22\n")
    disk = "package dep\n\n// Change is saved.\nfunc Change(value int, label string) {}\n"
    dependency = write(root / "dep/dep.go", disk)
    text = (
        'package main\nimport "example.test/help/dep"\n'
        'func run(callback func(before int)) { dep.Change(7, "x"); callback(7) }\n'
    )
    caller = write(root / "main.go", text)
    hover = site(caller, "dep.[[Change]](")
    call = site(caller, "Change([[7]],")
    with session(executable_path, root, timeout) as client:
        client.open_document(caller, text)
        saved = "func Change(value int, label string)"
        expect_editor_hover(client.request(hover_method, **editor_help_params(hover)), hover, saved, "Change is saved.")
        expect_editor_signature(client.request(signature_method, **editor_help_params(call)), saved, "Change is saved.")

        client.open_document(dependency, disk)
        client.change_document(dependency, 2, "package dep\n\n// Change is unsaved.\nfunc Change(next uint, label string) {}\n")
        edited = "func Change(next uint, label string)"
        expect_editor_hover(client.request(hover_method, **editor_help_params(hover)), hover, edited, "Change is unsaved.")
        expect_editor_signature(client.request(signature_method, **editor_help_params(call)), edited, "Change is unsaved.")
        client.close_document(dependency)
        expect_editor_hover(client.request(hover_method, **editor_help_params(hover)), hover, saved, "Change is saved.")
        expect_editor_signature(client.request(signature_method, **editor_help_params(call)), saved, "Change is saved.")

        changed = text.replace("func(before int)", "func(after uint)")
        for version, source, parameter in [(2, changed, "after uint"), (None, text, "before int")]:
            if version is None:
                client.close_document(caller)
            else:
                client.change_document(caller, version, source)
            hover_callback = site(caller, "; [[callback]](", source)
            call_callback = site(caller, "callback([[7]])", source)
            signature = f"func({parameter})"
            expect_editor_hover(
                client.request(hover_method, **editor_help_params(hover_callback)),
                hover_callback, f"var callback {signature}",
            )
            expect_editor_signature(client.request(signature_method, **editor_help_params(call_callback)), signature)


def check_editor_help_refresh(executable_path, root, timeout):
    signature_method = "textDocument/signatureHelp"
    hover_method = "textDocument/hover"
    gate = PausedGoList(root / "control", timeout)
    project = root / "project"

    def config(version):
        return (
            "module example.test/help\n\ngo 1.22\n"
            "require example.test/dep v0.0.0\n"
            f"replace example.test/dep => ./versions/{version}\n"
        )

    for version, ty in [("initial", "int"), ("obsolete", "string"), ("latest", "uint")]:
        dependency = project / "versions" / version
        write(dependency / "go.mod", "module example.test/dep\n\ngo 1.22\n")
        write(dependency / "dep.go", f"package dep\n\n// Run uses {version}.\nfunc Run(value {ty}) {{}}\n")
    go_mod = write(project / "go.mod", config("initial"))
    caller = write(
        project / "main.go", 'package main\nimport "example.test/dep"\n'
        "func Local(value int) {}\nfunc run() { dep.Run(7); Local(8) }\n",
    )
    hover = site(caller, "dep.[[Run]](")
    signature = site(caller, "Run([[7]])")
    local_hover = site(caller, "; [[Local]](")
    local_signature = site(caller, "Local([[8]])")

    with session(executable_path, project, timeout, watch_files=True, **gate.environment) as client:
        client.open_document(caller, hover.text)
        expect_editor_hover(
            client.request(hover_method, **editor_help_params(hover)),
            hover, "func Run(value int)", "Run uses initial.",
        )
        expect_editor_signature(
            client.request(signature_method, **editor_help_params(signature)),
            "func Run(value int)", "Run uses initial.",
        )
        with gate.pause():
            write(go_mod, config("obsolete"))
            notify_changes(client, (go_mod, 2))
            client.send(id="signature-refresh", method=signature_method, params=editor_help_params(signature))
            gate.wait_for_snapshot()
            client.send(id="hover-refresh", method=hover_method, params=editor_help_params(hover))
            write(go_mod, config("latest"))
            notify_changes(client, (go_mod, 2))
            client.send(id="local-hover", method=hover_method, params=editor_help_params(local_hover))
            expect_editor_hover(wait_for_response(client, "local-hover").get("result"), local_hover, "func Local(value int)")
            client.send(id="local-signature", method=signature_method, params=editor_help_params(local_signature))
            expect_editor_signature(wait_for_response(client, "local-signature").get("result"), "func Local(value int)")
        expect_editor_signature(
            wait_for_response(client, "signature-refresh").get("result"),
            "func Run(value uint)", "Run uses latest.",
        )
        expect_editor_hover(
            wait_for_response(client, "hover-refresh").get("result"),
            hover, "func Run(value uint)", "Run uses latest.",
        )


CASES = [
    ("editor-help-overlays", check_editor_help_overlays, 9),
    ("editor-help-refresh", check_editor_help_refresh, 10),
]
