"""Check watched workspace changes, package graphs, and graph refresh."""

from check_goland_symbols import WORKSPACE
from lsp_harness import PausedGoList, expect_error, notify_changes, session, site, wait_for_response, write

from ._assertions import (
    check_location, check_references, check_type_location, expect_target,
    expect_workspace_symbols, no_definition, send_definition,
)


def check_mutations(executable_path, root, timeout):
    def module_config(replacement):
        return (
            "module example.test/watch\n\ngo 1.22\n"
            "require example.test/remote v0.0.0\n"
            f"replace example.test/remote => ./versions/{replacement}\n"
        )

    go_mod = write(root / "go.mod", module_config("first"))
    caller = write(
        root / "caller.go",
        'package watcher\nimport ("example.test/watch/dep"; "example.test/remote")\n'
        "func call() { dep.First(); remote.Remote() }\n",
    )
    existing = write(root / "dep" / "initial.go", "package dep\n\nfunc First() {}\n")
    versions = []
    for directory in ("first", "second"):
        dependency = root / "versions" / directory
        write(dependency / "go.mod", "module example.test/remote\n\ngo 1.22\n")
        versions.append(write(dependency / "remote.go", "package remote\n\nfunc Remote() {}\n"))

    first = site(caller, "dep.[[First]]()")
    initial = site(existing, "func [[First]]()")
    remote = site(caller, "remote.[[Remote]]()")
    with session(executable_path, root, timeout, watch_files=True) as client:
        client.open_document(caller, first.text)
        check_location(client, first, initial)
        check_location(client, remote, site(versions[0], "func [[Remote]]()"))

        client.open_document(existing, initial.text)
        write(existing, "// moved declaration\n" + initial.text)
        notify_changes(client, (existing, 2))
        check_location(client, first, initial)
        client.close_document(existing)
        check_location(client, first, site(existing, "func [[First]]()"))

        created = write(root / "dep" / "created.go", "package dep\n\nfunc Added() {}\n")
        notify_changes(client, (created, 1))
        edited = first.text.replace("dep.First()", "dep.Added()")
        client.change_document(caller, 2, edited)
        added = site(caller, "dep.[[Added]]()", edited)
        added_target = site(created, "func [[Added]]()")
        check_location(client, added, added_target)

        write(created, "//go:build fixture && !fixture\n\n" + added_target.text)
        notify_changes(client, (created, 2))
        no_definition(client, added)
        write(created, added_target.text)
        notify_changes(client, (created, 2))
        check_location(client, added, added_target)

        renamed = created.with_name("renamed.go")
        created.rename(renamed)
        notify_changes(client, (created, 3), (renamed, 1))
        check_location(client, added, site(renamed, "func [[Added]]()"))

        renamed.unlink()
        notify_changes(client, (renamed, 3))
        no_definition(client, added)

        write(go_mod, module_config("second"))
        notify_changes(client, (go_mod, 2))
        check_location(client, site(caller, "remote.[[Remote]]()", edited), site(versions[1], "func [[Remote]]()"))


def check_go_work(executable_path, root, timeout):
    def workspace_config(library):
        return f"go 1.22\n\nuse (\n\t./app\n\t./{library}\n)\n"

    work_file = write(root / "go.work", workspace_config("lib"))
    write(root / "app" / "go.mod", "module example.test/app\n\ngo 1.22\n")
    caller = write(
        root / "app" / "caller.go",
        'package app\nimport "example.test/lib"\nfunc call() { lib.Run() }\n',
    )
    libraries = []
    for library in ("lib", "replacement"):
        write(root / library / "go.mod", "module example.test/lib\n\ngo 1.22\n")
        libraries.append(write(root / library / "lib.go", "package lib\n\nfunc Run() {}\n"))

    reference = site(caller, "lib.[[Run]]()")
    with session(executable_path, root, timeout, watch_files=True, GOWORK="") as client:
        client.open_document(caller, reference.text)
        first = site(libraries[0], "func [[Run]]()")
        replacement = site(libraries[1], "func [[Run]]()")
        check_location(client, reference, first)
        check_references(client, reference, (reference, first), include_declaration=True)
        expect_workspace_symbols(client.request(WORKSPACE, query="lib.Run"), first, kind=12, container="lib")
        write(work_file, workspace_config("replacement"))
        notify_changes(client, (work_file, 2))
        check_location(client, reference, replacement)
        check_references(client, reference, (reference, replacement), include_declaration=True)
        expect_workspace_symbols(client.request(WORKSPACE, query="lib.Run"), replacement, kind=12, container="lib")


def check_gopath(executable_path, root, timeout):
    package = root / "src" / "example.test" / "legacy"
    caller = write(
        package / "caller.go",
        'package legacy\nimport "example.test/legacy/dep"\nfunc call() { dep.Run() }\n',
    )
    declaration = write(
        package / "dep" / "dep.go",
        "package dep\n\ntype Result struct{}\nfunc Run() *Result { return nil }\n",
    )
    reference = site(caller, "dep.[[Run]]()")
    with session(executable_path, package, timeout, GO111MODULE="off", GOPATH=str(root)) as client:
        client.open_document(caller, reference.text)
        run = site(declaration, "func [[Run]]()")
        check_location(client, reference, run)
        check_type_location(client, reference, site(declaration, "type [[Result]] struct{}"))
        check_references(client, reference, (reference, run), include_declaration=True)


def check_async_refresh(executable_path, root, timeout):
    gate = PausedGoList(root / "control", timeout)
    project = root / "project"

    def module_config(replacement):
        return (
            "module example.test/refresh\n\ngo 1.22\n"
            "require example.test/remote v0.0.0\n"
            f"replace example.test/remote => ./versions/{replacement}\n"
        )

    go_mod = write(project / "go.mod", module_config("initial"))
    primary = write(
        project / "primary.go",
        'package refresh\nimport "example.test/remote"\n'
        "func Primary() { local := 1; _ = local; remote.Run() }\n",
    )
    editable = write(
        project / "editable.go",
        'package refresh\nimport "example.test/remote"\nfunc Editable() { remote.Run() }\n',
    )
    targets = {}
    for name in ("initial", "obsolete", "latest"):
        dependency = project / "versions" / name
        write(dependency / "go.mod", "module example.test/remote\n\ngo 1.22\n")
        target = write(dependency / "remote.go", "package remote\n\nfunc Run() {}\n")
        targets[name] = site(target, "func [[Run]]()")

    write(project / "changing" / "original.go", "package changing\n")
    unrelated = write(project / "unrelated" / "caller.go", "package unrelated\nfunc Call() { Keep() }\n")
    unrelated_target = write(project / "unrelated" / "keep.go", "package unrelated\nfunc Keep() {}\n")
    dependent = write(
        project / "dependent" / "caller.go",
        'package dependent\nimport "example.test/refresh/changing"\nfunc Call() { changing.Added() }\n',
    )

    graph_query = site(primary, "remote.[[Run]]()")
    lexical_query = site(primary, "_ = [[local]];")
    lexical_target = site(primary, "{ [[local]] :=")
    editable_query = site(editable, "remote.[[Run]]()")
    with session(executable_path, project, timeout, watch_files=True, **gate.environment) as client:
        client.open_document(primary, graph_query.text)
        client.open_document(editable, editable_query.text)
        check_location(client, graph_query, targets["initial"])
        initial_calls = gate.calls()

        with gate.pause():
            write(go_mod, module_config("obsolete"))
            notify_changes(client, (go_mod, 2))
            send_definition(client, "load-obsolete-graph", graph_query)
            gate.wait_for_snapshot()
            send_definition(client, "lexical-while-loading", lexical_query)
            expect_target(
                wait_for_response(client, "lexical-while-loading", min(timeout, 5)),
                lexical_target,
            )

            send_definition(client, "client-cancel", graph_query)
            client.notify("$/cancelRequest", id="client-cancel")
            expect_error(wait_for_response(client, "client-cancel", min(timeout, 5)), -32800)

            send_definition(client, "source-changed", editable_query)
            client.change_document(editable, 2, "// shifted cursor\n" + editable_query.text)
            expect_error(wait_for_response(client, "source-changed", min(timeout, 5)), -32801)

            write(go_mod, module_config("latest"))
            for _ in range(3):
                notify_changes(client, (go_mod, 2))
            # This response orders the server's processing before the relay is released.
            send_definition(client, "events-applied", lexical_query)
            expect_target(wait_for_response(client, "events-applied", min(timeout, 5)), lexical_target)

        expect_target(wait_for_response(client, "load-obsolete-graph"), targets["latest"])
        calls = gate.calls()
        if len(calls) - len(initial_calls) != 2:
            raise RuntimeError(f"expected an obsolete load and one coalesced replacement; got {calls!r}")

        with gate.pause():
            added = write(project / "changing" / "added.go", "package changing\nfunc Added() {}\n")
            notify_changes(client, (added, 1))
            gate.wait_for_snapshot()
            send_definition(client, "unaffected-package", site(unrelated, "{ [[Keep]]()"))
            expect_target(
                wait_for_response(client, "unaffected-package", min(timeout, 5)),
                site(unrelated_target, "func [[Keep]]()"),
            )
            send_definition(client, "changed-dependency", site(dependent, "changing.[[Added]]()"))
        expect_target(wait_for_response(client, "changed-dependency"), site(added, "func [[Added]]()"))

        gate.fail_next()
        notify_changes(client, (go_mod, 2))
        send_definition(client, "failed-load", graph_query)
        expect_error(wait_for_response(client, "failed-load"), -32803)
        check_location(client, graph_query, targets["latest"])


CASES = [
    ("mutations", check_mutations, 1),
    ("go-work", check_go_work, 2),
    ("gopath", check_gopath, 4),
    ("async-refresh", check_async_refresh, 5),
]
