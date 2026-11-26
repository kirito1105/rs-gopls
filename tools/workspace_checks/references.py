"""Check local and workspace references, including cancellation."""

from lsp_harness import PausedGoList, expect_error, notify_changes, session, site, wait_for_response, write

from ._assertions import check_references, reference_params


def check_reference_locals(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/reflocal\n\ngo 1.22\n")
    caller = write(
        root / "caller.go",
        "package reflocal\n\n"
        "func split(n int) (int, bool) { return n, true }\n"
        "func call() int {\n"
        "    value := 1\n"
        "    value, ok := split(value)\n"
        "    if !ok { return 0 }\n"
        "    {\n"
        "        value := value + 1\n"
        "        _ = value\n"
        "    }\n"
        "    /* \U0001f9ea */ return value\n"
        "}\n",
    )
    outer_decl = site(caller, "    [[value]] := 1")
    reused = site(caller, "    [[value]], ok :=")
    input_use = site(caller, "split([[value]])")
    shadow_decl = site(caller, "        [[value]] := value + 1")
    shadow_initializer = site(caller, "value := [[value]] + 1")
    shadow_use = site(caller, "        _ = [[value]]")
    outer_use = site(caller, "*/ return [[value]]")
    outer_uses = (reused, input_use, shadow_initializer, outer_use)

    with session(executable_path, root, timeout) as client:
        client.open_document(caller, outer_decl.text)
        check_references(client, reused, outer_uses)
        check_references(client, outer_use, (*outer_uses, outer_decl), include_declaration=True)
        check_references(client, shadow_initializer, outer_uses)
        check_references(client, shadow_decl, (shadow_use,))
        check_references(client, shadow_use, (shadow_decl, shadow_use), include_declaration=True)


def check_reference_workspace(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/refscope\n\ngo 1.22\n")
    target = write(root / "dep" / "target.go", "package dep\n\nfunc Run() {}\n")
    sibling = write(root / "dep" / "sibling.go", "package dep\n\nfunc localUse() { Run() }\n")
    internal = write(
        root / "dep" / "internal_test.go",
        "package dep\n\nfunc internalUse() { Run() }\n",
    )
    external = write(
        root / "dep" / "external_test.go",
        'package dep_test\nimport target "example.test/refscope/dep"\n'
        "func externalUse() { target.Run() }\n",
    )
    write(
        root / "other" / "other.go",
        "package other\n\ntype Other struct{}\nfunc (Other) Run() {}\nfunc Run() {}\n",
    )
    caller = write(
        root / "consumer" / "caller.go",
        'package consumer\nimport ("example.test/refscope/other"; target "example.test/refscope/dep")\n'
        "func Run() {}\n"
        "func call() { target.Run(); other.Run(); Run(); (other.Other{}).Run() }\n",
    )
    declaration = site(target, "func [[Run]]()")
    same_package = site(sibling, "{ [[Run]]()")
    internal_use = site(internal, "{ [[Run]]()")
    external_use = site(external, "target.[[Run]]()")
    cross_package = site(caller, "target.[[Run]]()")
    other_uses = (same_package, internal_use, external_use)

    draft_path = root / "dep" / "draft_use.go"
    draft_text = "package dep\n\nfunc draftUse() { Run() }\n"
    draft = site(draft_path, "{ [[Run]]()", draft_text)
    start = {"line": 0, "character": 0}
    prefix = "// shifted by an unsaved edit\n"
    shifted = site(caller, "target.[[Run]]()", prefix + cross_package.text)

    with session(executable_path, root, timeout, watch_files=True) as client:
        client.open_document(target, declaration.text)
        check_references(client, declaration, (*other_uses, cross_package))
        check_references(
            client,
            cross_package,
            (declaration, *other_uses, cross_package),
            include_declaration=True,
        )
        # Internal-test and external-test calls identify the same source declaration.
        check_references(client, internal_use, (*other_uses, cross_package))
        check_references(client, external_use, (*other_uses, cross_package))

        client.open_document(caller, cross_package.text)
        client.change_document_range(caller, 2, start, start, prefix)
        client.open_document(draft_path, draft.text)
        check_references(client, shifted, (*other_uses, shifted, draft))

        client.close_document(draft_path)
        check_references(client, declaration, (*other_uses, shifted))
        client.close_document(caller)
        check_references(client, declaration, (*other_uses, cross_package))

        sibling.unlink()
        notify_changes(client, (sibling, 3))
        check_references(client, declaration, (internal_use, external_use, cross_package))

    if draft_path.exists():
        raise RuntimeError(f"test overlay unexpectedly wrote {draft_path}")


def check_reference_cancel(executable_path, root, timeout):
    gate = PausedGoList(root / "control", timeout)
    project = root / "project"
    go_mod = write(project / "go.mod", "module example.test/refcancel\n\ngo 1.22\n")
    write(project / "dep" / "target.go", "package dep\n\nfunc Run() {}\n")
    source = (
        'package refcancel\nimport "example.test/refcancel/dep"\n'
        "func run() { dep.Run() }\n"
    )
    primary = site(write(project / "primary.go", source), "dep.[[Run]]()")
    editable = site(
        write(project / "editable.go", source.replace("func run()", "func edit()")),
        "dep.[[Run]]()",
    )
    prefix = "// shifted while metadata was pending\n"
    shifted = site(editable.path, "dep.[[Run]]()", prefix + editable.text)
    start = {"line": 0, "character": 0}

    with session(executable_path, project, timeout, watch_files=True, **gate.environment) as client:
        client.open_document(primary.path, primary.text)
        client.open_document(editable.path, editable.text)
        check_references(client, primary, (primary, editable))

        with gate.pause():
            write(go_mod, go_mod.read_text(encoding="utf-8") + "\n// refresh the package graph\n")
            notify_changes(client, (go_mod, 2))
            client.send(id="cancel-refs", method="textDocument/references", params=reference_params(primary))
            gate.wait_for_snapshot()
            client.notify("$/cancelRequest", id="cancel-refs")
            expect_error(wait_for_response(client, "cancel-refs", min(timeout, 5)), -32800)

            client.send(id="changed-refs", method="textDocument/references", params=reference_params(editable))
            client.change_document_range(editable.path, 2, start, start, prefix)
            expect_error(wait_for_response(client, "changed-refs", min(timeout, 5)), -32801)

        check_references(client, shifted, (primary, shifted))


def check_active_reference_cancel(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/refbusy\n\ngo 1.22\n")
    declaration = write(root / "target.go", "package refbusy\n\nfunc Target() {}\nfunc Marker() {}\n")
    target = site(declaration, "func [[Target]]()")
    marker = site(declaration, "func [[Marker]]()")
    uses = []
    for number in range(192):
        caller = write(
            root / f"caller_{number:03d}.go",
            f"package refbusy\n\nfunc use{number}() {{ Target() }}\n",
        )
        uses.append(site(caller, "{ [[Target]]()"))

    with session(executable_path, root, timeout) as client:
        # An exact nonlocal search first ensures the test-inclusive graph is ready.
        check_references(client, marker, ())
        client.send(id="active-refs", method="textDocument/references", params=reference_params(target, False))
        client.notify("$/cancelRequest", id="active-refs")
        expect_error(wait_for_response(client, "active-refs"), -32800)

        first = uses[0]
        edited = "// unsaved shift\n" + first.text
        shifted = site(first.path, "{ [[Target]]()", edited)
        client.send(id="dirty-refs", method="textDocument/references", params=reference_params(target, False))
        client.open_document(first.path, edited)
        expect_error(wait_for_response(client, "dirty-refs"), -32801)
        check_references(client, target, (shifted, *uses[1:]))


CASES = [
    ("reference-locals", check_reference_locals, 14),
    ("reference-workspace", check_reference_workspace, 15),
    ("reference-cancel", check_reference_cancel, 16),
    ("reference-active-cancel", check_active_reference_cancel, 17),
]
