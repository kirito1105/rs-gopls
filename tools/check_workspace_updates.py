#!/usr/bin/env python3
"""Verify navigation updates across Go workspace and filesystem changes."""

import argparse
from contextlib import contextmanager
import math
import os
from pathlib import Path
from queue import Empty
import shutil
import subprocess
import sys
import tempfile
import time

from check_definitions import Site, check_location
from check_goland_definitions import normalize_locations
from check_goland_implementations import METHOD as IMPLEMENTATION_METHOD
from check_goland_references import Location
from compare_definitions import LspClient, executable


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8", newline="\n")
    return path


def site(path, anchor, text=None):
    if text is None:
        text = path.read_text(encoding="utf-8")
    before, selected = anchor.split("[[", 1)
    token, after = selected.split("]]", 1)
    try:
        start = text.index(before + token + after) + len(before)
    except ValueError as exc:
        raise RuntimeError(f"could not find {anchor!r} in {path}") from exc
    return Site(path, text, start, start + len(token))


@contextmanager
def session(executable_path, root, timeout, *, watch_files=False, **env):
    environment = {**os.environ, "GO111MODULE": "on", "GOWORK": "off", "GOPROXY": "off", **env}
    client = LspClient([executable_path], root, timeout, environment=environment)
    try:
        client.initialize(watch_files=watch_files)
        if watch_files:
            acknowledge_file_watches(client)
        yield client
        client.shutdown()
    except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
        log = client.stderr_tail()
        raise RuntimeError(f"{root}: {exc}" + (f"\n{log}" if log else "")) from exc
    finally:
        client.close()

def notify_changes(client, *events):
    client.notify(
        "workspace/didChangeWatchedFiles",
        changes=[{"uri": path.as_uri(), "type": event} for path, event in events],
    )


def no_definition(client, reference):
    response = client.request(
        "textDocument/definition",
        textDocument={"uri": reference.path.as_uri()},
        position=reference.cursor(),
    )
    if response:
        raise RuntimeError(f"expected no definition for the excluded symbol, received {response!r}")


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
        write(work_file, workspace_config("replacement"))
        notify_changes(client, (work_file, 2))
        check_location(client, reference, replacement)
        check_references(client, reference, (reference, replacement), include_declaration=True)


def check_test_packages(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/widget\n\ngo 1.22\n")
    implementation = write(root / "widget.go", "package widget\n\nfunc private() {}\nfunc Exported() {}\n")
    test_helper = write(root / "helper_test.go", "package widget\n\nfunc testOnly() {}\n")
    internal_test = write(
        root / "widget_test.go",
        'package widget\nimport "testing"\n'
        "func TestInternal(t *testing.T) { private(); testOnly() }\n",
    )
    external_test = write(
        root / "external_test.go",
        'package widget_test\nimport ("testing"; "example.test/widget")\n'
        "func TestExternal(t *testing.T) { widget.Exported() }\n",
    )
    private = site(internal_test, "{ [[private]]();")
    test_only = site(internal_test, "; [[testOnly]]()")
    public = site(external_test, "widget.[[Exported]]()")
    with session(executable_path, root, timeout) as client:
        client.open_document(internal_test, private.text)
        client.open_document(external_test, public.text)
        check_location(client, private, site(implementation, "func [[private]]()"))
        check_location(client, test_only, site(test_helper, "func [[testOnly]]()"))
        check_location(client, public, site(implementation, "func [[Exported]]()"))


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



def next_message(client, deadline):
    try:
        message = client.inbox.get(timeout=max(0, deadline - time.monotonic()))
    except Empty as exc:
        raise RuntimeError("timed out waiting for an LSP message") from exc
    if isinstance(message, Exception):
        raise RuntimeError(f"LSP transport: {message}") from message
    return message


def acknowledge_file_watches(client):
    deadline = time.monotonic() + client.timeout
    while True:
        message = next_message(client, deadline)
        method = message.get("method")
        if method == "client/registerCapability":
            registrations = message["params"]["registrations"]
            patterns = {
                watcher["globPattern"]
                for registration in registrations
                if registration["method"] == "workspace/didChangeWatchedFiles"
                for watcher in registration["registerOptions"]["watchers"]
            }
            required = {"**/*.go", "**/go.mod", "**/go.sum", "**/go.work", "**/go.work.sum"}
            if not required.issubset(patterns):
                raise RuntimeError(f"missing Go file watcher registration: {required - patterns}")
            client.reply(message)
            return
        if method is not None and "id" in message:
            client.reply(message)


def send_definition(client, request_id, reference):
    client.send(
        id=request_id, method="textDocument/definition",
        params={"textDocument": {"uri": reference.path.as_uri()}, "position": reference.cursor()},
    )


def wait_for_response(client, request_id, timeout=None):
    deadline = time.monotonic() + (client.timeout if timeout is None else timeout)
    while True:
        try:
            message = next_message(client, deadline)
        except RuntimeError as exc:
            raise RuntimeError(f"waiting for {request_id}: {exc}") from exc
        if "method" in message:
            if "id" in message:
                client.reply(message)
            continue
        if message.get("id") != request_id:
            raise RuntimeError(
                f"expected response {request_id!r} first, received {message.get('id')!r} "
                f"with {response_detail(message)}"
            )
        return message


def expect_target(response, target):
    if "error" in response:
        raise RuntimeError(f"expected definition in {target.path}, received {response['error']}")
    expect_location(response["result"], target)


def check_type_location(client, reference, target):
    result = client.request(
        "textDocument/typeDefinition",
        textDocument={"uri": reference.path.as_uri()},
        position=reference.cursor(),
    )
    expect_location(result, target)


def expect_location(result, target):
    locations = normalize_locations(result)
    if (
        len(locations) != 1
        or Path(locations[0]["path"]) != target.path
        or locations[0]["range"] != target.selection()
    ):
        raise RuntimeError(f"expected {target.path} {target.selection()}, received {locations!r}")


def response_detail(response):
    error = response.get("error")
    if error is not None:
        return f"error {error!r}"
    result = response.get("result")
    return f"success with {len(result)} items" if isinstance(result, list) else f"success with {result!r}"


def expect_error(response, code):
    if response.get("error", {}).get("code") != code:
        raise RuntimeError(
            f"expected LSP error {code} for {response.get('id')!r}, received {response_detail(response)}"
        )


GO_LIST_RELAY = r"""package main

import (
    "bytes"
    "fmt"
    "os"
    "os/exec"
    "path/filepath"
    "time"
)

func main() {
    cmd := exec.Command(os.Getenv("RS_GOPLS_TEST_REAL_GO"), os.Args[1:]...)
    cmd.Stdin, cmd.Stdout, cmd.Stderr = os.Stdin, os.Stdout, os.Stderr
    if len(os.Args) < 2 || os.Args[1] != "list" {
        os.Exit(run(cmd))
    }

    control := os.Getenv("RS_GOPLS_TEST_GO_GATE")
    log, err := os.OpenFile(filepath.Join(control, "calls"), os.O_WRONLY|os.O_CREATE|os.O_APPEND, 0600)
    if err != nil {
        fail(err)
    }
    if _, err := fmt.Fprintln(log, "list"); err != nil {
        fail(err)
    }
    log.Close()
    if os.Remove(filepath.Join(control, "fail")) == nil {
        fail(fmt.Errorf("injected go list failure"))
    }
    if os.Remove(filepath.Join(control, "arm")) != nil {
        os.Exit(run(cmd))
    }

    // Capture metadata before signaling that caller and build config may change.
    var stdout, stderr bytes.Buffer
    cmd.Stdout, cmd.Stderr = &stdout, &stderr
    code := run(cmd)
    if err := os.WriteFile(filepath.Join(control, "ready"), nil, 0600); err != nil {
        fail(err)
    }

    deadline := time.Now().Add(30 * time.Second)
    for {
        if _, err := os.Stat(filepath.Join(control, "release")); err == nil {
            break
        }
        if time.Now().After(deadline) {
            fail(fmt.Errorf("timed out waiting for the Go list test gate"))
        }
        time.Sleep(10 * time.Millisecond)
    }
    os.Stdout.Write(stdout.Bytes())
    os.Stderr.Write(stderr.Bytes())
    os.Exit(code)
}

func run(cmd *exec.Cmd) int {
    if err := cmd.Run(); err != nil {
        if exit, ok := err.(*exec.ExitError); ok {
            return exit.ExitCode()
        }
        fail(err)
    }
    return 0
}

func fail(err error) {
    fmt.Fprintln(os.Stderr, err)
    os.Exit(1)
}
"""


def go_list_relay(root, timeout):
    real_go = shutil.which("go")
    if not real_go:
        raise RuntimeError("Go executable not found for the workspace loader relay")
    relay_file = write(root / "relay.go", GO_LIST_RELAY)
    relay_path = root / ("go.exe" if os.name == "nt" else "go")
    subprocess.run(
        [real_go, "build", "-o", str(relay_path), str(relay_file)],
        cwd=root, env={**os.environ, "GO111MODULE": "off", "GOWORK": "off", "GOPROXY": "off"},
        check=True, timeout=timeout,
    )
    return {
        "PATH": str(root) + os.pathsep + os.environ["PATH"],
        "RS_GOPLS_TEST_REAL_GO": str(Path(real_go).resolve()),
        "RS_GOPLS_TEST_GO_GATE": str(root),
    }


def wait_for_snapshot(control, timeout):
    deadline = time.monotonic() + timeout
    while not (control / "ready").is_file():
        if time.monotonic() >= deadline:
            raise RuntimeError("go list relay did not capture a pending snapshot")
        time.sleep(0.01)


def check_async_refresh(executable_path, root, timeout):
    control = root / "control"
    environment = go_list_relay(control, timeout)
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
    with session(executable_path, project, timeout, watch_files=True, **environment) as client:
        client.open_document(primary, graph_query.text)
        client.open_document(editable, editable_query.text)
        check_location(client, graph_query, targets["initial"])
        initial_calls = (control / "calls").read_text(encoding="utf-8").splitlines()

        write(control / "arm", "")
        write(go_mod, module_config("obsolete"))
        notify_changes(client, (go_mod, 2))
        send_definition(client, "load-obsolete-graph", graph_query)

        try:
            wait_for_snapshot(control, timeout)
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
        finally:
            write(control / "release", "")

        expect_target(wait_for_response(client, "load-obsolete-graph"), targets["latest"])
        calls = (control / "calls").read_text(encoding="utf-8").splitlines()
        if len(calls) - len(initial_calls) != 2:
            raise RuntimeError(f"expected an obsolete load and one coalesced replacement; got {calls!r}")

        (control / "ready").unlink()
        (control / "release").unlink()
        write(control / "arm", "")
        added = write(project / "changing" / "added.go", "package changing\nfunc Added() {}\n")
        notify_changes(client, (added, 1))
        try:
            wait_for_snapshot(control, timeout)
            send_definition(client, "unaffected-package", site(unrelated, "{ [[Keep]]()"))
            expect_target(
                wait_for_response(client, "unaffected-package", min(timeout, 5)),
                site(unrelated_target, "func [[Keep]]()"),
            )
            send_definition(client, "changed-dependency", site(dependent, "changing.[[Added]]()"))
        finally:
            write(control / "release", "")
        expect_target(wait_for_response(client, "changed-dependency"), site(added, "func [[Added]]()"))

        write(control / "fail", "")
        notify_changes(client, (go_mod, 2))
        send_definition(client, "failed-load", graph_query)
        expect_error(wait_for_response(client, "failed-load"), -32803)
        check_location(client, graph_query, targets["latest"])


def check_overlay_new_file(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/overlay\n\ngo 1.22\n")
    caller = write(
        root / "caller.go",
        "package overlay\n\nfunc Existing() {}\nfunc Caller() { Existing(); Added() }\n",
    )
    internal = write(root / "internal_test.go", "package overlay\n\nfunc CallerTest() { Added() }\n")
    external = write(root / "external_test.go", "package overlay_test\n\nfunc CallerTest() { Added() }\n")
    internal_call = site(internal, "{ [[Added]]()")
    external_call = site(external, "{ [[Added]]()")
    virtual = root / "helper_extra.go"
    virtual_text = "package overlay\n\nfunc Added() { Existing() }\n"
    existing = site(caller, "func [[Existing]]()")
    existing_call = site(caller, "{ [[Existing]]();")
    added_call = site(caller, "; [[Added]]()")
    added = site(virtual, "func [[Added]]()", virtual_text)
    virtual_existing_call = site(virtual, "{ [[Existing]]()", virtual_text)
    with session(executable_path, root, timeout) as client:
        client.open_document(caller, existing.text)
        check_location(client, existing_call, existing)
        no_definition(client, added_call)
        client.open_document(virtual, virtual_text)
        check_location(client, added_call, added)
        check_location(client, internal_call, added)
        no_definition(client, external_call)
        check_location(client, virtual_existing_call, existing)
        client.close_document(virtual)
        no_definition(client, added_call)
        no_definition(client, internal_call)
    if virtual.exists():
        raise RuntimeError(f"test overlay unexpectedly wrote {virtual}")

def check_overlay_disk_restore(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/overlayrestore\n\ngo 1.22\n")
    caller = write(
        root / "caller.go",
        'package overlayrestore\nimport "example.test/overlayrestore/dep"\n'
        "func Caller() { dep.Run() }\n",
    )
    saved_text = "package dep\n\n// marker\nfunc Run() {}\n"
    target_path = write(root / "dep" / "target.go", saved_text)
    reference = site(caller, "dep.[[Run]]()")
    saved = site(target_path, "func [[Run]]()")
    overlay = site(target_path, "func [[Run]]()", "// unsaved overlay\n" + saved_text)

    with session(executable_path, root, timeout) as client:
        client.open_document(caller, reference.text)
        check_location(client, reference, saved)

        client.open_document(target_path, saved.text)
        client.change_document(target_path, 2, overlay.text)
        check_location(client, reference, overlay)
        client.close_document(target_path)
        check_location(client, reference, saved)

        client.open_document(target_path, overlay.text)
        original_stat = target_path.stat()
        # Keep file size and modification time unchanged while moving the declaration.
        write(target_path, "package dep\n\nfunc Run() {}\n// marker\n")
        os.utime(target_path, ns=(original_stat.st_atime_ns, original_stat.st_mtime_ns))
        check_location(client, reference, overlay)
        client.close_document(target_path)
        check_location(client, reference, site(target_path, "func [[Run]]()"))


def check_type_overlay(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/typeoverlay\n\ngo 1.22\n")
    caller = write(
        root / "caller.go",
        'package overlay\nimport "example.test/typeoverlay/dep"\n'
        "func call() {\n"
        "    value := dep.New()\n"
        "    /* \U0001f9ea */ _ = value\n"
        "    {\n"
        "        var value dep.Shadow\n"
        "        _ = value\n"
        "    }\n"
        "}\n",
    )
    write(root / "dep" / "factory.go", "package dep\n\nfunc New() *Item { return nil }\n")
    types = write(root / "dep" / "types.go", "package dep\n\ntype Item struct{}\ntype Shadow struct{}\n")
    inferred = site(caller, "*/ _ = [[value]]")
    shadowed = site(caller, "\n        _ = [[value]]")
    inferred_binding = site(caller, "[[value]] := dep.New()")
    shadowed_binding = site(caller, "var [[value]] dep.Shadow")
    item = site(types, "type [[Item]] struct{}")
    shadow = site(types, "type [[Shadow]] struct{}")

    with session(executable_path, root, timeout) as client:
        client.open_document(caller, inferred.text)
        check_location(client, inferred, inferred_binding)
        check_location(client, shadowed, shadowed_binding)
        check_type_location(client, inferred, item)
        check_type_location(client, shadowed, shadow)

        client.open_document(types, item.text)
        prefix = "// moved in the open buffer\n"
        start = {"line": 0, "character": 0}
        client.change_document_range(types, 2, start, start, prefix)
        edited = prefix + item.text
        check_type_location(client, inferred, site(types, "type [[Item]] struct{}", edited))
        check_type_location(client, shadowed, site(types, "type [[Shadow]] struct{}", edited))

        client.close_document(types)
        check_type_location(client, inferred, item)
        check_type_location(client, shadowed, shadow)



def reference_params(query, include_declaration=False):
    return {
        "textDocument": {"uri": query.path.as_uri()},
        "position": query.cursor(),
        "context": {"includeDeclaration": include_declaration},
    }


def check_references(client, query, expected, *, include_declaration=False):
    def location_key(path, span):
        start, end = span["start"], span["end"]
        return path, start["line"], start["character"], end["line"], end["character"]

    result = client.request("textDocument/references", **reference_params(query, include_declaration))
    if result is not None and not isinstance(result, list):
        raise RuntimeError(f"expected an array of references, received {result!r}")
    actual = [
        location_key(Path(location["path"]), location["range"])
        for location in normalize_locations(result)
    ]
    targets = sorted(location_key(target.path, target.selection()) for target in expected)
    if actual != targets:
        raise RuntimeError(
            f"references from {query.path} at {query.cursor()} "
            f"(includeDeclaration={include_declaration}): expected {targets!r}, got {actual!r}"
        )


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
    control = root / "control"
    environment = go_list_relay(control, timeout)
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

    with session(executable_path, project, timeout, watch_files=True, **environment) as client:
        client.open_document(primary.path, primary.text)
        client.open_document(editable.path, editable.text)
        check_references(client, primary, (primary, editable))

        write(control / "arm", "")
        write(go_mod, go_mod.read_text(encoding="utf-8") + "\n// refresh the package graph\n")
        notify_changes(client, (go_mod, 2))
        client.send(id="cancel-refs", method="textDocument/references", params=reference_params(primary))
        try:
            wait_for_snapshot(control, timeout)
            client.notify("$/cancelRequest", id="cancel-refs")
            expect_error(wait_for_response(client, "cancel-refs", min(timeout, 5)), -32800)

            client.send(id="changed-refs", method="textDocument/references", params=reference_params(editable))
            client.change_document_range(editable.path, 2, start, start, prefix)
            expect_error(wait_for_response(client, "changed-refs", min(timeout, 5)), -32801)
        finally:
            write(control / "release", "")

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


def implementation_params(query):
    return {"textDocument": {"uri": query.path.as_uri()}, "position": query.cursor()}


def check_implementations(client, query, *expected):
    response = client.request(IMPLEMENTATION_METHOD, **implementation_params(query))
    actual = tuple(
        Location.from_range(Path(location["path"]), location["range"])
        for location in normalize_locations(response)
    )
    targets = {Location.from_range(target.path, target.selection()) for target in expected}
    locations = set(actual)
    if len(actual) != len(targets) or locations != targets:
        missing = sorted(targets - locations)
        extra = sorted(locations - targets)
        raise RuntimeError(
            f"implementations from {query.path} at {query.cursor()}: expected {len(targets)} locations, "
            f"got {len(actual)}. Missing {len(missing)} {missing[:3]!r}, extra {len(extra)} {extra[:3]!r}, "
            f"duplicates {len(actual) - len(locations)}"
        )
    return actual


def check_implementation_updates(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/implupdates\n\ngo 1.22\n")
    origin = write(root / "worker.go", "package jobs\n\ntype Worker interface { Work(int) }\n")
    method = site(origin, "{ [[Work]](int)")
    interface = site(origin, "type [[Worker]] interface")
    implementation = write(
        root / "impl" / "worker.go", "package impl\n\ntype Live struct{}\nfunc (*Live) Work(int) {}\n",
    )
    internal = write(
        implementation.with_name("worker_test.go"),
        "package impl\n\ntype Internal struct{}\nfunc (Internal) Work(int) {}\n",
    )
    external = write(
        implementation.with_name("external_test.go"),
        "package impl_test\n\ntype External struct{}\nfunc (External) Work(int) {}\n",
    )
    live_type = site(implementation, "type [[Live]] struct")
    live = site(implementation, "(*Live) [[Work]](int)")
    internal_method = site(internal, "(Internal) [[Work]](int)")
    external_method = site(external, "(External) [[Work]](int)")
    draft_path = implementation.with_name("draft.go")
    draft = site(draft_path, "(Draft) [[Work]](int)", "package impl\n\ntype Draft struct{}\nfunc (Draft) Work(int) {}\n")
    prefix = "// shifted in the unsaved implementation\n"
    first = {"line": 0, "character": 0}
    shifted = site(implementation, "(*Live) [[Work]](int)", prefix + live.text)
    initial = (live, internal_method, external_method)

    with session(executable_path, root, timeout, watch_files=True) as client:
        client.open_document(origin, method.text)
        original_order = check_implementations(client, method, *initial)
        if check_implementations(client, method, *initial) != original_order:
            raise RuntimeError("identical implementation queries returned different orders")
        check_implementations(
            client, interface, live_type, site(internal, "type [[Internal]] struct"),
            site(external, "type [[External]] struct"),
        )

        client.open_document(implementation, live.text)
        client.change_document_range(implementation, 2, first, first, prefix)
        check_implementations(client, method, shifted, internal_method, external_method)
        client.change_document(implementation, 3, live.text.replace("Work(int)", "Work(string)"))
        client.open_document(draft_path, draft.text)
        check_implementations(client, method, draft, internal_method, external_method)
        client.close_document(draft_path)
        client.close_document(implementation)
        check_implementations(client, method, *initial)

        changed_source = method.text.replace("Work(int)", "Work(string)")
        client.change_document(origin, 2, changed_source)
        check_implementations(client, site(origin, "{ [[Work]](string)", changed_source))
        client.close_document(origin)
        check_implementations(client, method, *initial)

        write(implementation, "//go:build fixture && !fixture\n\n" + live.text)
        notify_changes(client, (implementation, 2))
        check_implementations(client, method, internal_method, external_method)
        write(implementation, live.text)
        notify_changes(client, (implementation, 2))
        check_implementations(client, method, *initial)

    if draft_path.exists():
        raise RuntimeError(f"test overlay unexpectedly wrote {draft_path}")


def check_implementation_candidate_packages(executable_path, root, timeout):
    module = "example.test/implpruning"
    write(root / "go.mod", f"module {module}\n\ngo 1.22\n")
    interface = site(
        write(root / "contract" / "worker.go", "package contract\n\ntype Worker interface { DeliverParcel() }\n"),
        "type [[Worker]] interface",
    )
    direct = site(
        write(
            root / "courier" / "delivery.go",
            "package courier\n\ntype Courier struct{}\nfunc (Courier) DeliverParcel() {}\n",
        ),
        "type [[Courier]] struct",
    )
    wrapper = site(
        write(
            root / "bridge" / "bridge.go",
            f'package bridge\n\nimport "{module}/courier"\n\ntype Bridge struct {{ courier.Courier }}\n',
        ),
        "type [[Bridge]] struct",
    )
    alias = site(
        write(
            root / "exported" / "alias.go",
            f'package exported\n\nimport "{module}/bridge"\n\ntype Exported = bridge.Bridge\n',
        ),
        "type [[Exported]] =",
    )
    editable = write(root / "switcher" / "switcher.go", "package switcher\n\ntype Switch struct{}\n")
    importer = site(
        write(
            root / "transit" / "transit.go",
            f'package transit\n\nimport "{module}/switcher"\n\ntype Transit struct {{ switcher.Switch }}\n',
        ),
        "type [[Transit]] struct",
    )
    draft = site(
        editable,
        "type [[Switch]] struct",
        f'package switcher\n\nimport "{module}/exported"\n\ntype Switch struct {{ exported.Exported }}\n',
    )
    method = site(direct.path, "(Courier) [[DeliverParcel]]()")
    interface_method = site(interface.path, "{ [[DeliverParcel]]()")
    on_disk = (direct, wrapper, alias)

    with session(executable_path, root, timeout) as client:
        client.open_document(interface.path, interface.text)
        check_implementations(client, interface, *on_disk)
        check_implementations(client, method, interface_method)

        client.open_document(editable, draft.text)
        check_implementations(client, interface, *on_disk, draft, importer)
        client.close_document(editable)
        check_implementations(client, interface, *on_disk)


def check_implementation_cancel(executable_path, root, timeout):
    control = root / "control"
    environment = go_list_relay(control, timeout)
    project = root / "project"
    go_mod = write(project / "go.mod", "module example.test/implcancel\n\ngo 1.22\n")
    origin = site(write(project / "worker.go", "package jobs\n\ntype Worker interface { Work() }\n"), "{ [[Work]]()")
    target = site(
        write(project / "impl" / "worker.go", "package impl\n\ntype Worker struct{}\nfunc (Worker) Work() {}\n"),
        "(Worker) [[Work]]()",
    )
    shifted = site(origin.path, "{ [[Work]]()", "// changed while metadata loaded\n" + origin.text)

    with session(executable_path, project, timeout, watch_files=True, **environment) as client:
        client.open_document(origin.path, origin.text)
        check_implementations(client, origin, target)
        write(control / "arm", "")
        write(go_mod, go_mod.read_text(encoding="utf-8") + "\n// refresh the package graph\n")
        notify_changes(client, (go_mod, 2))
        try:
            client.send(id="cancel-impl", method=IMPLEMENTATION_METHOD, params=implementation_params(origin))
            wait_for_snapshot(control, timeout)
            client.notify("$/cancelRequest", id="cancel-impl")
            expect_error(wait_for_response(client, "cancel-impl", min(timeout, 5)), -32800)

            client.send(id="changed-impl", method=IMPLEMENTATION_METHOD, params=implementation_params(origin))
            client.change_document(origin.path, 2, shifted.text)
            expect_error(wait_for_response(client, "changed-impl", min(timeout, 5)), -32801)
        finally:
            write(control / "release", "")
        check_implementations(client, shifted, target)


def check_active_implementation_cancel(executable_path, root, timeout):
    write(root / "go.mod", "module example.test/implbusy\n\ngo 1.22\n")
    origin = site(write(root / "worker.go", "package jobs\n\ntype Worker interface { Work() }\n"), "{ [[Work]]()")
    methods = []
    for number in range(384):
        worker = f"Worker{number:03d}"
        path = write(root / f"worker_{number:03d}.go", f"package jobs\ntype {worker} struct{{}}\nfunc ({worker}) Work() {{}}\n")
        methods.append(site(path, f"({worker}) [[Work]]()"))

    def start_active_search(client, request_id):
        # The barrier response confirms that the search was dispatched and is still pending.
        barrier_id = f"{request_id}-barrier"
        client.send_batch(
            {"id": request_id, "method": IMPLEMENTATION_METHOD, "params": implementation_params(origin)},
            {"id": barrier_id, "method": "rs-gopls/testUnknownMethod", "params": {}},
        )
        expect_error(wait_for_response(client, barrier_id), -32601)

    with session(executable_path, root, timeout) as client:
        client.open_document(origin.path, origin.text)
        check_implementations(client, origin, *methods)
        # Keep the loaded package graph and invalidate the previous query result.
        client.change_document(origin.path, 2, origin.text + "\ntype Unrelated struct{}\n")
        start_active_search(client, "active-impl")
        client.notify("$/cancelRequest", id="active-impl")
        expect_error(wait_for_response(client, "active-impl"), -32800)

        first = methods[0]
        prefix = "// shifted in the open buffer\n"
        shifted = site(first.path, "(Worker000) [[Work]]()", prefix + first.text)
        start_active_search(client, "dirty-impl")
        client.open_document(first.path, shifted.text)
        expect_error(wait_for_response(client, "dirty-impl"), -32801)
        check_implementations(client, origin, shifted, *methods[1:])


CASES = {
    "mutations": check_mutations,
    "go-work": check_go_work,
    "test-packages": check_test_packages,
    "gopath": check_gopath,
    "async-refresh": check_async_refresh,
    "overlay-new-file": check_overlay_new_file,
    "overlay-disk-restore": check_overlay_disk_restore,
    "type-overlay": check_type_overlay,
    "reference-locals": check_reference_locals,
    "reference-workspace": check_reference_workspace,
    "reference-cancel": check_reference_cancel,
    "reference-active-cancel": check_active_reference_cancel,
    "implementation-updates": check_implementation_updates,
    "implementation-candidate-packages": check_implementation_candidate_packages,
    "implementation-cancel": check_implementation_cancel,
    "implementation-active-cancel": check_active_implementation_cancel,
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=executable)
    parser.add_argument("--case", choices=tuple(CASES), action="append", help="run only this case; may repeat")
    parser.add_argument("--timeout", type=float, default=60.0)
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be positive and finite")

    failures = 0
    with tempfile.TemporaryDirectory(prefix="rs-gopls-workspace-check-") as directory:
        root = Path(directory).resolve()
        for name in dict.fromkeys(args.case or CASES):
            try:
                CASES[name](args.candidate, root / name, args.timeout)
            except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
                print(f"FAIL {name}: {exc}", file=sys.stderr)
                failures += 1
            else:
                print(f"PASS {name}")
    return int(bool(failures))


if __name__ == "__main__":
    raise SystemExit(main())
