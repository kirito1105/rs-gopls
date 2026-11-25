#!/usr/bin/env python3
"""Verify definition updates across Go workspace and filesystem changes."""

import argparse
from contextlib import contextmanager
import math
from pathlib import Path
import subprocess
import sys
import tempfile
from unittest.mock import patch

from check_definitions import Site, check_location
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
def session(executable_path, root, timeout, **env):
    spawn = subprocess.Popen

    def spawn_with_environment(*args, **kwargs):
        kwargs["env"] = {**kwargs["env"], "GO111MODULE": "on", **env}
        return spawn(*args, **kwargs)

    # Reuse the normal client and override only its test process environment.
    with patch("compare_definitions.subprocess.Popen", side_effect=spawn_with_environment):
        client = LspClient([executable_path], root, timeout)
    try:
        client.initialize()
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
    with session(executable_path, root, timeout) as client:
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
    with session(executable_path, root, timeout, GOWORK="") as client:
        client.open_document(caller, reference.text)
        check_location(client, reference, site(libraries[0], "func [[Run]]()"))
        write(work_file, workspace_config("replacement"))
        notify_changes(client, (work_file, 2))
        check_location(client, reference, site(libraries[1], "func [[Run]]()"))


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
    declaration = write(package / "dep" / "dep.go", "package dep\n\nfunc Run() {}\n")
    reference = site(caller, "dep.[[Run]]()")
    with session(executable_path, package, timeout, GO111MODULE="off", GOPATH=str(root)) as client:
        client.open_document(caller, reference.text)
        check_location(client, reference, site(declaration, "func [[Run]]()"))


CASES = {
    "mutations": check_mutations,
    "go-work": check_go_work,
    "test-packages": check_test_packages,
    "gopath": check_gopath,
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
