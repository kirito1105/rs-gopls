#!/usr/bin/env python3
"""Run exact Go definition checks against a native LSP executable."""

import argparse
from dataclasses import dataclass
import math
from pathlib import Path
import re
import subprocess
import sys
import tempfile

from compare_definitions import LspClient, executable, path_from_uri, position_at


@dataclass(frozen=True)
class Site:
    path: Path
    text: str
    start: int
    end: int

    def cursor(self):
        return position_at(self.text, min(self.start + 1, self.end - 1))

    def selection(self):
        return {
            "start": position_at(self.text, self.start),
            "end": position_at(self.text, self.end),
        }


MARKER = re.compile(r"\[\[([a-zA-Z0-9_-]+):([^\]\r\n]+)\]\]")
FIXTURE = {
    "go.mod": "module example.test/definition\n\ngo 1.22\n",
    "main.go": """package main

import helper [[import:"example.test/definition/service"]]

type API struct {
    [[service-field:Service]] *helper.Service
}

func (api *API) [[initialize:initialize]]() {}

func check([[parameter:argument]] string) {
    outer := [[parameter-use:argument]]
    {
        [[shadow-declaration:outer]] := "shadow"
        /* 🙂 */ _ = [[shadow-use:outer]]
    }
    _ = outer
    [[private-use:registerRoutes]]()
    var _ *[[private-type-use:context]]
    helper.[[imported-function:Create]]()

    api := &API{}
    api.[[method-use:initialize]]()
    _ = api.[[field-use:Service]]
    api.Service.[[field-method:Run]]()
    wrapper := &helper.Service{}
    wrapper.[[short-method:Run]]()
    var explicit *helper.Service
    explicit.[[explicit-method:Run]]()

    helper.Create().Next().[[chained-method:Run]]()
    created, _ := helper.Pair()
    created.[[tuple-method:Run]]()
    var embedded helper.Embedded
    embedded.[[embedded-method:Run]]()
    var interfaceTyped helper.Runner
    interfaceTyped.[[interface-method:Run]]()
    var alias helper.Alias
    alias.[[alias-method:Run]]()
    generic := helper.Box[string]{}
    _ = generic.[[generic-field:Value]]
    typed := helper.Box[*helper.Service]{}
    typed.Value.[[generic-field-method:Run]]()
    _ = (*helper.Service).[[pointer-method-expression:Run]]
}

func main() {}
""",
    "helpers.go": """package main

type [[private-type:context]] struct{}
func [[private-function:registerRoutes]]() {}
""",
    "service/service.go": """package [[service-package:service]]

type Service struct{}
type Alias = Service
type Embedded struct{ *Service }
type Runner interface{ [[interface-declaration:Run]]() }
type Box[T any] struct{ [[generic-declaration:Value]] T }

func (s *Service) [[run:Run]]() {}
func (s *Service) Next() *Service { return s }
func [[create:Create]]() *Service { return &Service{} }
func Pair() (*Service, error) { return Create(), nil }
""",
}
FIXTURE_CASES = (
    (1, "parameter-use", "parameter"),
    (1, "shadow-use", "shadow-declaration"),
    (1, "private-use", "private-function"),
    (1, "private-type-use", "private-type"),
    (1, "import", "service-package"),
    (1, "imported-function", "create"),
    (2, "method-use", "initialize"),
    (2, "field-use", "service-field"),
    (2, "field-method", "run"),
    (2, "short-method", "run"),
    (2, "explicit-method", "run"),
    (3, "chained-method", "run"),
    (3, "tuple-method", "run"),
    (3, "embedded-method", "run"),
    (3, "interface-method", "interface-declaration"),
    (3, "alias-method", "run"),
    (3, "generic-field", "generic-declaration"),
    (3, "generic-field-method", "run"),
    (3, "pointer-method-expression", "run"),
)


def write_fixture(root):
    sites = {}
    for relative, source in FIXTURE.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        spans = {}

        parts = []
        previous = 0
        offset = 0
        for match in MARKER.finditer(source):
            preceding = source[previous:match.start()]
            label, token = match.groups()
            offset += len(preceding)
            spans[label] = (offset, offset + len(token))
            offset += len(token)
            parts.extend([preceding, token])
            previous = match.end()
        parts.append(source[previous:])
        text = "".join(parts)
        path.write_text(text, encoding="utf-8", newline="\n")
        for label, (start, end) in spans.items():
            sites[label] = Site(path, text, start, end)
    return sites


def check_location(client, source, target):
    response = client.request(
        "textDocument/definition",
        textDocument={"uri": source.path.as_uri()},
        position=source.cursor(),
    )
    locations = response if isinstance(response, list) else [response] if response else []
    if len(locations) == 1:
        result = locations[0]
        uri = result.get("targetUri", result.get("uri"))
        selection = result.get("targetSelectionRange", result.get("range"))
        expected = target.selection()
        starts = [expected["start"]]
        # Import definitions can select either the package name or its whole clause.
        if source.text[source.start] == '"' and target.text[target.start - 8:target.start] == "package ":
            starts.append(position_at(target.text, target.start - 8))
        if (
            uri and path_from_uri(uri) == target.path and selection
            and selection["start"] in starts and selection["end"] == expected["end"]
        ):
            return
    raise RuntimeError(f"expected {target.path} at {target.selection()}, received {response!r}")


def exercise(command, root, cases, timeout, overlay=None):
    client = LspClient([command], root, timeout)
    failures = []
    try:
        client.initialize()
        for path, text in {source.path: source.text for _, source, _ in cases}.items():
            client.open_document(path, text)

        def check(label, source, target):
            try:
                check_location(client, source, target)
            except RuntimeError as exc:
                failures.append(f"{label}: {exc}")

        for label, source, target in cases:
            check(label, source, target)

        if overlay:
            source, target = overlay
            prefix = "// unsaved buffer line\n"
            if all(origin.path != target.path for _, origin, _ in cases):
                client.open_document(target.path, target.text)
            client.change_document(target.path, 2, prefix + target.text)
            moved = Site(target.path, prefix + target.text, target.start + len(prefix), target.end + len(prefix))
            check("unsaved target", source, moved)
            client.close_document(target.path)
            check("restored disk target", source, target)

        client.shutdown()
    finally:
        client.close()

    count = len(cases) + (2 if overlay else 0)
    for failure in failures:
        print(f"FAIL {failure}", file=sys.stderr)
    print(f"{root}: {count - len(failures)}/{count} definitions matched")
    return not failures


API = "internal/api/api.go"
CONTEXT = "internal/api/context.go"
WORKSPACE_CASES = (
    (1, "Context", "func(c *[[Context]],", CONTEXT, "type [[Context]] struct"),
    (1, "API", "&[[API]]{", API, "type [[API]] struct"),
    (1, "Routes type", "Routes *[[Routes]]", API, "type [[Routes]] struct"),
    (1, "corsMiddleware", "router.Use([[corsMiddleware]])", API, "func [[corsMiddleware]]("),
    (1, "execPath", "filepath.Dir([[execPath]])", API, "[[execPath]], _ := os.Executable()"),
    (1, "WriteError", "[[WriteError]](w, model.NewNotFoundError", CONTEXT, "func [[WriteError]]("),
    (1, "GetMuxVars", "Params: [[GetMuxVars]](r)", CONTEXT, "func [[GetMuxVars]]("),
    (2, "Routes field", "api.[[Routes]].APIRoot = router", API, "[[Routes]] *Routes"),
    (2, "APIRoot", "api.Routes.[[APIRoot]] = router", API, "[[APIRoot]] *mux.Router"),
    (2, "initUser", "api.[[initUser]]()", "internal/api/user.go", "func (api *API) [[initUser]]("),
    (2, "GetSession", "api.app.[[GetSession]](token)", "internal/app/session.go", "func (a *App) [[GetSession]]("),
    (3, "Send", "conn.[[Send]](hello.ToJSON())", "internal/web/websocket.go", "func (wc *WebConn) [[Send]]("),
    (3, "Config", "c.App.[[Config]]().Update", "internal/app/app.go", "func (a *App) [[Config]]("),
)


def locate(path, template):
    text = path.read_text(encoding="utf-8")
    before, token, after = re.fullmatch(r"(.+?|)\[\[(.+?)\]\](.*)", template).groups()
    parts = [r"\s+".join(re.escape(word) for word in re.split(r"\s+", piece)) for piece in (before, after)]
    match = re.search(parts[0] + f"({re.escape(token)})" + parts[1], text)
    if not match:
        raise RuntimeError(f"could not find {template!r} in {path}")
    return Site(path, text, *match.span(1))


def workspace_cases(root, stage):
    return [
        (label, locate(root / API, source), locate(root / target_path, target))
        for minimum, label, source, target_path, target in WORKSPACE_CASES
        if minimum <= stage
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=executable)
    parser.add_argument("--stage", type=int, choices=(1, 2, 3), default=1)
    parser.add_argument("--workspace", type=Path, help="localchat/server root for additional real-world checks")
    parser.add_argument("--timeout", type=float, default=60.0)
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be positive and finite")

    try:
        with tempfile.TemporaryDirectory(prefix="rs-gopls-definition-check-") as directory:
            root = Path(directory).resolve()
            sites = write_fixture(root)
            cases = [
                (source, sites[source], sites[target])
                for stage, source, target in FIXTURE_CASES
                if stage <= args.stage
            ]
            success = exercise(
                args.candidate, root, cases, args.timeout,
                overlay=(sites["private-use"], sites["private-function"]),
            )
        if args.workspace:
            workspace = args.workspace.resolve()
            success = exercise(
                args.candidate, workspace, workspace_cases(workspace, args.stage), args.timeout
            ) and success
    except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
        print(f"Definition check failed: {exc}", file=sys.stderr)
        return 1
    return int(not success)


if __name__ == "__main__":
    raise SystemExit(main())
