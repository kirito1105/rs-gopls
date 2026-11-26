#!/usr/bin/env python3
"""Check Go call signatures and parameter highlights over stdio LSP."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import math
from pathlib import Path
import subprocess
import sys
import tempfile

from check_goland_definitions import fixture_path, utf16_offset
from check_goland_hover import DEFAULT_FIXTURE, DIRECTIVE
from compare_definitions import LspClient, executable, position_at


METHOD = "textDocument/signatureHelp"

# GoLand displays the method-expression receiver as *Box[T] and drops the
# formal name of the second parameter. LSP presents the actual receiver and
# retains the name from the method declaration.
METHOD_EXPRESSION_PARAMETERS = {
    "syn-sig-method-expr-p0": ("*helper.Box[int]", "v int"),
    "syn-sig-method-expr-p1": ("*helper.Box[int]", "v int"),
}
METHOD_EXPRESSION_BASELINE = ("*Box[T]", "int")

DOC_FRAGMENTS = {
    "sig-stdlib-variadic-p0": "Errorf formats according to a format specifier",
    "sig-stdlib-variadic-p1": "Errorf formats according to a format specifier",
    "sig-nested-inner": "ToLower returns",
    "sig-nested-outer": "TrimSpace returns a slice",
    "sig-builtin-append-p1": "appends elements to the end of a slice",
    "sig-builtin-make-p1": "allocates and initializes",
    "syn-sig-grouped-p0": "Add sums two integers with a label.",
    "syn-sig-grouped-p1": "Add sums two integers with a label.",
    "syn-sig-grouped-p2": "Add sums two integers with a label.",
    "syn-sig-variadic-p0": "Join concatenates parts separated by sep.",
    "syn-sig-variadic-p1": "Join concatenates parts separated by sep.",
    "syn-sig-variadic-p3-clamped": "Join concatenates parts separated by sep.",
    "syn-sig-generic-call-p0": "Map transforms each element using fn.",
    "syn-sig-generic-method-p0": "Set replaces the stored value.",
    "syn-sig-method-expr-p0": "Set replaces the stored value.",
    "syn-sig-method-expr-p1": "Set replaces the stored value.",
    "syn-sig-iface-variadic-p2": "Format renders args using prefix.",
    "syn-sig-named-func-type-p0": "UnaryOp transforms a single integer.",
    "syn-sig-builtin-make-p2": "allocates and initializes",
    "syn-sig-builtin-append-p3": "appends elements to the end of a slice",
}


@dataclass(frozen=True)
class Case:
    id: str
    path: Path
    position: dict[str, int]
    active: int | None
    item: dict | None


def load_cases(fixture, workspace):
    data = json.loads(fixture.read_text(encoding="utf-8-sig"))
    sources = {}
    cases = {}
    for row in data["cases"]:
        if row["kind"] != "signature":
            continue
        case_id = row["id"]
        try:
            if not case_id or case_id in cases:
                raise ValueError("empty or duplicate case ID")
            path = fixture_path(row["path"], workspace)
            if path not in sources:
                sources[path] = path.read_text(encoding="utf-8")
            position = {"line": row["line"], "character": row["character"]}
            offset = utf16_offset(sources[path], position["line"], position["character"])
            if not sources[path].startswith(row["leafText"], offset):
                raise ValueError(f"source at {path} {position} no longer starts with {row['leafText']!r}")
            active, items = row["activeParameter"], row["items"]
            if items:
                if len(items) != 1 or type(active) is not int or active < 0:
                    raise ValueError("expected one enabled item and a raw active index")
                item = items[0]
                if not item["enabled"]:
                    raise ValueError("expected an enabled item")
                utf16_slice(item["text"].encode("utf-16-le"), item["highlightStart"], item["highlightEnd"])
            elif active is None:
                item = None
            else:
                raise ValueError("empty item list has a non-null active index")
            cases[case_id] = Case(case_id, path, position, active, item)
        except (KeyError, OSError, TypeError, ValueError) as exc:
            raise ValueError(f"{fixture} case {case_id!r}: {exc}") from exc
    if not cases:
        raise ValueError(f"{fixture}: no signature cases")
    return cases.values(), sources


def utf16_slice(units, start, end):
    if type(start) is not int or type(end) is not int or not 0 <= start < end <= len(units) // 2:
        raise ValueError(f"invalid UTF-16 span {[start, end]!r}")
    try:
        return units[2 * start : 2 * end].decode("utf-16-le")
    except UnicodeDecodeError as exc:
        raise ValueError(f"UTF-16 span {[start, end]!r} splits a surrogate pair") from exc


def signature_parts(response):
    if (not isinstance(response, dict) or type(response.get("activeSignature")) is not int
            or response["activeSignature"] != 0):
        raise ValueError(f"expected a SignatureHelp object with activeSignature 0, received {response!r}")
    signatures = response.get("signatures")
    if not isinstance(signatures, list) or len(signatures) != 1 or not isinstance(signatures[0], dict):
        raise ValueError(f"expected one signature, received {signatures!r}")
    signature = signatures[0]
    label = signature.get("label")
    params = signature.get("parameters")
    if not isinstance(label, str) or not label.startswith("func") or not isinstance(params, list):
        raise ValueError(f"invalid signature {signature!r}")

    units = label.encode("utf-16-le")
    previous = None
    labels = []
    for parameter in params:
        span = parameter.get("label") if isinstance(parameter, dict) else None
        if not isinstance(span, list) or len(span) != 2:
            raise ValueError(f"expected an offset label, received {span!r}")
        start, end = span
        text = utf16_slice(units, start, end)
        if not text.strip() or text != text.strip():
            raise ValueError(f"parameter label includes whitespace at its boundary: {text!r}")
        if previous is None:
            separator = units[2 * (start - 1) : 2 * start] if start else b""
            expected = b"(\0"
        else:
            separator = units[2 * previous : 2 * start]
            expected = b",\0 \0"
        if separator != expected:
            raise ValueError(f"{span!r} does not select the next parameter in {label!r}")
        labels.append(text)
        previous = end
    if previous is not None and units[2 * previous : 2 * previous + 2] != b")\0":
        raise ValueError(f"last parameter does not end at the closing delimiter in {label!r}")

    doc = signature.get("documentation")
    if doc is not None and (
        not isinstance(doc, dict) or doc.get("kind") != "markdown" or not isinstance(doc.get("value"), str)
    ):
        raise ValueError(f"expected Markdown documentation, received {doc!r}")
    doc = doc["value"] if doc else ""
    if DIRECTIVE.search(doc) or any(directive in doc for directive in ("go:noinline", "go:build")):
        raise ValueError("compiler or GoLand directive leaked into documentation")
    active = response.get("activeParameter")
    if (labels and (type(active) is not int or active < 0)) or (not labels and active is not None):
        raise ValueError(f"invalid activeParameter {active!r}")
    return tuple(labels), active, doc


def check_case(case, response):
    if case.item is None:
        if response is not None:
            raise ValueError(f"expected null, received {response!r}")
        return
    parameters, active, doc = signature_parts(response)
    override = METHOD_EXPRESSION_PARAMETERS.get(case.id)
    recorded = case.item["text"]
    if override is not None:
        if parameters != override:
            raise ValueError(f"method expression expected {override!r}, got {parameters!r}")
        baseline = METHOD_EXPRESSION_BASELINE
    else:
        baseline = parameters
    if ", ".join(baseline) != recorded:
        raise ValueError(f"GoLand parameters {recorded!r}, server {', '.join(parameters)!r}")

    expected = case.active
    if parameters and (parameters[-1].startswith("...") or " ..." in parameters[-1]):
        expected = min(expected, len(parameters) - 1)
    if active != expected:
        raise ValueError(f"expected activeParameter {expected}, received {active}")
    if active >= len(baseline):
        raise ValueError(f"no GoLand parameter for active index {active}")
    prefix = ", ".join(baseline[:active])
    start = len(prefix.encode("utf-16-le")) // 2 + (2 if active else 0)
    end = start + len(baseline[active].encode("utf-16-le")) // 2
    if (start, end) != (case.item["highlightStart"], case.item["highlightEnd"]):
        raise ValueError(f"parameter {active} does not match GoLand's highlight in {recorded!r}")
    fragment = DOC_FRAGMENTS.get(case.id)
    if fragment is not None and fragment not in " ".join(doc.split()):
        raise ValueError(f"missing GoDoc fragment {fragment!r}")


def params(path, position):
    return {"textDocument": {"uri": path.as_uri()}, "position": position}


def check_editing(candidate, timeout):
    from check_workspace_updates import (
        expect_error, go_list_relay, session, wait_for_response, wait_for_snapshot, write,
    )

    with tempfile.TemporaryDirectory(prefix="rs-gopls-signature-edit-") as directory:
        root = Path(directory).resolve()
        control = root / "control"
        env = go_list_relay(control, timeout)
        project = root / "project"
        write(project / "go.mod", "module example.test/signature\n\ngo 1.22\n")
        definition = (
            "package dep\n\n"
            "// Foo reports whether an item is valid.\n"
            "func Foo(first int, second string) bool { return true }\n\n"
            "type S struct{}\n"
            "func (S) Method(x int, y string) {}\n"
        )
        dependency = write(project / "dep/dep.go", definition)
        header = 'package main\nimport "example.test/signature/dep"\nfunc run(s dep.S) { '
        disk = header + 'dep.Foo(1, "x") }\n'
        caller = write(project / "main.go", disk)
        query = params(caller, position_at(disk, disk.index("dep.Foo(") + len("dep.Foo(")))
        with session(candidate, project, timeout, **env) as client:
            client.open_document(caller, disk)
            write(control / "arm", "")
            client.send(id="cancel-signature", method=METHOD, params=query)
            try:
                wait_for_snapshot(control, timeout)
                client.notify("$/cancelRequest", id="cancel-signature")
                expect_error(wait_for_response(client, "cancel-signature"), -32800)
            finally:
                write(control / "release", "")

            def expect(parameters, active, doc_fragment=None):
                result = client.request(METHOD, **query)
                try:
                    labels, index, doc = signature_parts(result)
                except ValueError as exc:
                    raise RuntimeError(
                        f"expected {parameters!r} at {query['position']} during editing: {exc}"
                    ) from exc
                if (labels, index) != (parameters, active) or (doc_fragment and doc_fragment not in doc):
                    raise RuntimeError(f"unexpected signature during editing: {result!r}")

            expect(("first int", "second string"), 0, "Foo reports whether")
            version = 1
            for call, parameters, active in [
                ("dep.Foo(", ("first int", "second string"), 0),
                ("dep.Foo(1, ", ("first int", "second string"), 1),
                ("s.Method(1, ", ("x int", "y string"), 1),
            ]:
                buffer = header + call
                # Also recover when the rest of the containing block is present.
                version += 1
                client.change_document(caller, version, buffer + "\n}\n")
                query = params(caller, position_at(buffer, len(buffer)))
                expect(parameters, active)
            version += 1
            client.change_document(caller, version, disk)
            query = params(caller, position_at(disk, disk.index("dep.Foo(") + len("dep.Foo(")))
            client.open_document(dependency, definition)
            client.change_document(dependency, 2, definition.replace("first int", "𐐀 uint")
                .replace("Foo reports whether an item is valid.", "Foo uses the edited buffer."))
            expect(("𐐀 uint", "second string"), 0, "Foo uses the edited buffer.")
            client.close_document(dependency)
            expect(("first int", "second string"), 0, "Foo reports whether")
            client.close_document(caller)
            expect(("first int", "second string"), 0, "Foo reports whether")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=executable)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, default=DEFAULT_FIXTURE)
    parser.add_argument("--timeout", type=float, default=60.0)
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be positive and finite")

    try:
        workspace = args.workspace.resolve()
        cases, sources = load_cases(args.fixture, workspace)
        client = LspClient([args.candidate], workspace, args.timeout)
        try:
            provider = client.initialize().get("signatureHelpProvider")
            if (not isinstance(provider, dict) or provider.get("triggerCharacters") != ["(", ","]
                    or provider.get("retriggerCharacters") != [","]):
                raise RuntimeError(f"invalid signatureHelpProvider {provider!r}")
            for path, text in sources.items():
                client.open_document(path, text)
            failures = 0
            for case in cases:
                try:
                    check_case(case, client.request(METHOD, **params(case.path, case.position)))
                except (OSError, RuntimeError, ValueError) as exc:
                    print(f"{case.id}: {exc}", file=sys.stderr)
                    failures += 1
                else:
                    print(f"{case.id}: {'expected null' if case.item is None else 'matched'}")
            client.shutdown()
        finally:
            client.close()
        print(f"{len(cases) - failures}/{len(cases)} signature cases passed")
        check_editing(args.candidate, args.timeout)
        print("Signature editing, close restore, and cancellation passed")
    except (KeyError, OSError, RuntimeError, ValueError, subprocess.SubprocessError) as exc:
        print(f"GoLand signature check failed: {exc}", file=sys.stderr)
        return 1
    return int(failures != 0)


if __name__ == "__main__":
    raise SystemExit(main())
