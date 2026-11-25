#!/usr/bin/env python3
"""Run exact Go definition checks against a native LSP executable."""

import argparse
from dataclasses import dataclass
import math
import os
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
import [[default-import:"fmt"]]

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
    [[default-import-use:fmt]].Println()

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

var [[package-shadow:float32]] = 1
var _ = [[package-shadow-use:float32]]
var _ [[builtin-int:int]]
var _ = [[builtin-len:len]]([]int{})
var _ = [[builtin-true:true]]
var _ = [[builtin-false:false]]
var _ *int = [[builtin-nil:nil]]
const _ = [[builtin-iota:iota]]

func checkShadow() {
    [[local-shadow:len]] := 1
    _ = [[local-shadow-use:len]]
    [[literal-shadow:true]] := 1
    _ = [[literal-shadow-use:true]]
}
""",
    "callables.go": """package main

type FirstResult struct { [[first-result-field:First]] string }
type SecondResult struct { [[second-result-field:Second]] string }
type Callback func() FirstResult
type AliasCallback = Callback
type CallbackReceiver struct{}

func (CallbackReceiver) Build() FirstResult { return FirstResult{} }
func (*CallbackReceiver) BuildPointer() FirstResult { return FirstResult{} }
func makeFactory() func() FirstResult { return func() FirstResult { return FirstResult{} } }

func callables(r CallbackReceiver, callback Callback, alias AliasCallback) {
    [[callback-binding:local]] := func() (FirstResult, SecondResult) { return FirstResult{}, SecondResult{} }
    first, second := [[callback-binding-use:local]]()
    _ = first.[[local-callback-first:First]]
    _ = second.[[local-callback-second:Second]]
    _ = callback().[[typed-callback:First]]
    _ = alias().[[alias-callback:First]]
    type LocalCallback func() FirstResult
    var typedLocal LocalCallback
    _ = typedLocal().[[local-type-callback:First]]
    var anonymous struct{ Run func() FirstResult }
    _ = anonymous.Run().[[callback-field:First]]

    bound := r.Build
    _ = bound().[[method-value-result:First]]
    call := CallbackReceiver.Build
    _ = call(r).[[method-expression-result:First]]
    boundPointer := r.BuildPointer
    _ = boundPointer().[[pointer-method-value-result:First]]
    callPointer := (*CallbackReceiver).BuildPointer
    _ = callPointer(&r).[[pointer-method-expression-result:First]]

    _ = makeFactory()().[[higher-order-result:First]]
    _ = (func() FirstResult { return FirstResult{} })().[[literal-call-result:First]]
    _ = firstFromFactory.[[cross-file-var-first:Text]]
    _ = secondFromFactory.[[cross-file-var-second:Message]]
    _ = packageClosure().[[cross-file-closure:Text]]
    _ = packageFactory()().[[cross-file-higher-order:Text]]
    _ = packageInlineCallback().[[cross-file-inline-callback:Count]]

    {
        type FirstResult struct { Other int }
        specific := func() FirstResult { return FirstResult{} }
        _ = specific().[[shadowed-result-type:First]]
    }

    { local := 7; _ = local().[[non-function-shadow:First]] }
    _ = (&callback)().[[pointer-callback-no-call:First]]
    _ = cycleA().[[initializer-cycle:First]]
}
""",
    "callable_vars.go": """package main

import provider "example.test/definition/service"

var firstFromFactory, secondFromFactory = provider.NewRecords()
var packageClosure = func() provider.Record { return provider.Record{} }
var packageFactory = func() func() provider.Record { return packageClosure }
var packageInlineCallback = func() struct { [[inline-callback-field:Count]] int } { panic("unused") }

var cycleA = cycleB
var cycleB = cycleA
""",
    "containers.go": """package main
import helper "example.test/definition/service"

type Item struct {
    [[container-field:Label]] string
    [[container-view:View]] struct{ [[nested-inline-field:Title]] string }
}
func (*Item) [[container-pointer-method:Set]]() {}
type NamedItems []Item
type NamedArray [1]Item
type NamedMap map[string]Item
type DefinedItem Item
type DefinedRunner helper.Runner
type EmbeddedItem struct { Item }
type DefinedEmbeddedItem EmbeddedItem
type NamedChan <-chan Item
type mapKey string
const [[literal-map-key-const:entry]] mapKey = "entry"

func getItems() NamedItems { return nil }
func containers(sendOnly chan <-Item, ch NamedChan, values NamedMap, array *NamedArray) {
    _ = getItems()[0].[[slice-index-field:Label]]
    _ = array[0].[[pointer-array-field:Label]]
    _ = values["entry"].[[map-index-field:Label]]
    found, _ := values["entry"]
    _ = found.[[map-assignment:Label]]
    for _, item := range getItems() {
        _ = item.[[range-slice-field:Label]]
    }
    for _, item := range values {
        _ = item.[[range-map-field:Label]]
    }
    mapKeys := map[Item]int{}
    for item := range mapKeys {
        _ = item.[[range-map-key:Label]]
    }
    for item := range ch {
        _ = item.[[range-channel:Label]]
    }
    select {
    case item, _ := <-ch:
        _ = item.[[select-channel:Label]]
        item.[[select-pointer-method:Set]]()
    default:
    }
    fromChannel := <-ch
    _ = fromChannel.[[receive-channel:Label]]
    _ = (<-sendOnly).[[send-only-none:Label]]
    getItems()[0].[[slice-addressable:Set]]()
    values["entry"].[[map-not-addressable:Set]]()
    found.[[map-copy-addressable:Set]]()
    var pointerValues map[string]*Item
    pointerValues["entry"].[[map-pointer-value:Set]]()
    ([1]Item{})[0].[[array-not-addressable:Set]]()
    array[0].[[pointer-array-addressable:Set]]()

    var nested struct{ [[inline-field:Inner]] Item }
    _ = nested.[[anonymous-selector:Inner]]
    _ = nested.Inner.[[anonymous-chain:Label]]
    _ = found.[[anonymous-field-chain:View]].[[anonymous-nested:Title]]
    _ = helper.Inline().[[cross-file-inline:Entry]]
    _ = helper.Inline().Entry.[[cross-file-inline-chain:Text]]
    {
        _ = (struct{ /* 🙂 */ [[inline-shadow-left:Same]] int }{}).[[inline-shadow-left-use:Same]]
    }
    {
        _ = (struct{ [[inline-shadow-right:Same]] string }{}).[[inline-shadow-right-use:Same]]
    }

    _ = DefinedItem{}.[[defined-field:Label]]
    DefinedItem{}.[[defined-method-absent:Set]]()
    _ = DefinedEmbeddedItem{}.[[defined-promoted-field:Label]]
    var runner DefinedRunner
    runner.[[defined-interface-method:Run]]()
    var inlineRunner interface{ [[inline-interface-decl:Run]]() Item }
    _ = inlineRunner.[[inline-interface-method:Run]]().[[inline-interface-result:Label]]

    _ = []Item{{[[elided-struct-field:Label]]: "one"}}
    _ = map[mapKey]Item{[[literal-map-key-use:entry]]: {[[elided-map-field:Label]]: "two"}}
    _ = map[Item]int{{[[elided-map-key-field:Label]]: "three"}: 3}
}
""",
    "helpers.go": """package main

type [[private-type:context]] struct{}
func [[private-function:registerRoutes]]() {}
""",
    "service/service.go": """package service

type [[service-type:Service]] struct{}
type Alias = Service
type Embedded struct{ *Service }
type Runner interface{ [[interface-declaration:Run]]() }
type Box[T any] struct{ [[generic-declaration:Value]] T }

func (s *Service) [[run:Run]]() {}
func (s *Service) Next() *Service { return s }
func [[create:Create]]() *Service { return &Service{} }
func Pair() (*Service, error) { return Create(), nil }

type Record struct{ [[cross-file-inline-final:Text]] string }
type Message struct{ [[cross-file-second-result:Message]] string }
func NewRecords() (Record, Message) { return Record{}, Message{} }
func Inline() struct{ [[cross-file-inline-field:Entry]] Record } { panic("placeholder") }
""",
    "dotuse/dotuse.go": """package dotuse
import . "example.test/definition/service"
var _ = [[dot-function:Create]]()
var _ *[[dot-type:Service]]
""",
    "collision/collision.go": """package collision
func Create() {}
""",
    "ambiguous/ambiguous.go": """package ambiguous
import (
    . "example.test/definition/service"
    . "example.test/definition/collision"
)
var _ = [[dot-ambiguous:Create]]
""",
}
FIXTURE_CASES = (
    (1, "parameter-use", "parameter"),
    (1, "shadow-use", "shadow-declaration"),
    (1, "private-use", "private-function"),
    (1, "private-type-use", "private-type"),
    (1, "import", "import"),
    (1, "imported-function", "create"),
    (1, "default-import-use", "default-import"),
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
    (3, "local-shadow-use", "local-shadow"),
    (3, "literal-shadow-use", "literal-shadow"),
    (3, "package-shadow-use", "package-shadow"),
    (3, "dot-function", "create"),
    (3, "dot-type", "service-type"),
    (3, "dot-ambiguous", None),
    (4, "slice-index-field", "container-field"),
    (4, "pointer-array-field", "container-field"),
    (4, "map-index-field", "container-field"),
    (4, "map-assignment", "container-field"),
    (4, "range-slice-field", "container-field"),
    (4, "range-map-field", "container-field"),
    (4, "range-map-key", "container-field"),
    (4, "range-channel", "container-field"),
    (4, "select-channel", "container-field"),
    (4, "select-pointer-method", "container-pointer-method"),
    (4, "receive-channel", "container-field"),
    (4, "send-only-none", None),
    (4, "slice-addressable", "container-pointer-method"),
    (4, "map-not-addressable", None),
    (4, "map-copy-addressable", "container-pointer-method"),
    (4, "map-pointer-value", "container-pointer-method"),
    (4, "array-not-addressable", None),
    (4, "pointer-array-addressable", "container-pointer-method"),
    (4, "anonymous-selector", "inline-field"),
    (4, "anonymous-chain", "container-field"),
    (4, "anonymous-field-chain", "container-view"),
    (4, "anonymous-nested", "nested-inline-field"),
    (4, "cross-file-inline", "cross-file-inline-field"),
    (4, "cross-file-inline-chain", "cross-file-inline-final"),
    (4, "inline-shadow-left-use", "inline-shadow-left"),
    (4, "inline-shadow-right-use", "inline-shadow-right"),
    (4, "defined-field", "container-field"),
    (4, "defined-method-absent", None),
    (4, "defined-promoted-field", "container-field"),
    (4, "defined-interface-method", "interface-declaration"),
    (4, "inline-interface-method", "inline-interface-decl"),
    (4, "inline-interface-result", "container-field"),
    (4, "elided-struct-field", "container-field"),
    (4, "literal-map-key-use", "literal-map-key-const"),
    (4, "elided-map-field", "container-field"),
    (4, "elided-map-key-field", "container-field"),
    (5, "callback-binding-use", "callback-binding"),
    (5, "local-callback-first", "first-result-field"),
    (5, "local-callback-second", "second-result-field"),
    (5, "typed-callback", "first-result-field"),
    (5, "alias-callback", "first-result-field"),
    (5, "local-type-callback", "first-result-field"),
    (5, "callback-field", "first-result-field"),
    (5, "method-value-result", "first-result-field"),
    (5, "method-expression-result", "first-result-field"),
    (5, "pointer-method-value-result", "first-result-field"),
    (5, "pointer-method-expression-result", "first-result-field"),
    (5, "higher-order-result", "first-result-field"),
    (5, "literal-call-result", "first-result-field"),
    (5, "cross-file-var-first", "cross-file-inline-final"),
    (5, "cross-file-var-second", "cross-file-second-result"),
    (5, "cross-file-closure", "cross-file-inline-final"),
    (5, "cross-file-higher-order", "cross-file-inline-final"),
    (5, "cross-file-inline-callback", "inline-callback-field"),
    (5, "shadowed-result-type", None),
    (5, "non-function-shadow", None),
    (5, "pointer-callback-no-call", None),
    (5, "initializer-cycle", None),
)
BUILTIN_CASES = (
    ("builtin-int", "type [[int]] int"),
    ("builtin-len", "func [[len]]("),
    ("builtin-true", "[[true]] = 0 == 0"),
    ("builtin-false", "[[false]] = 0 != 0"),
    ("builtin-nil", "var [[nil]] Type"),
    ("builtin-iota", "const [[iota]] = 0"),
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
    if target is None:
        if not locations:
            return
        raise RuntimeError(f"expected no unambiguous definition, received {response!r}")
    if len(locations) == 1:
        result = locations[0]
        uri = result.get("targetUri", result.get("uri"))
        selection = result.get("targetSelectionRange", result.get("range"))
        expected = target.selection()
        if (
            uri and path_from_uri(uri) == target.path and selection
            and selection == expected
        ):
            return
    raise RuntimeError(f"expected {target.path} at {target.selection()}, received {response!r}")


def exercise(command, root, cases, timeout, overlays=()):
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

        for source, target in overlays:
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

    count = len(cases) + 2 * len(overlays)
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


def builtin_cases(root, sites):
    goroot = subprocess.check_output(
        ["go", "env", "GOROOT"], cwd=root, encoding="utf-8",
        env={**os.environ, "GOWORK": "off", "GOPROXY": "off"},
    ).strip()
    builtin = (Path(goroot) / "src" / "builtin" / "builtin.go").resolve()
    return [(source, sites[source], locate(builtin, target)) for source, target in BUILTIN_CASES]


CONTAINER_WORKSPACE_CASES = (
    ("range member", "internal/app/user.go", "u.[[Sanitize]]()",
     "internal/model/user.go", "func (u *User) [[Sanitize]]("),
    ("slice index member", "internal/app/card_action_test.go", "tree[0].[[AttachmentID]]",
     "internal/app/card_action.go", "[[AttachmentID]] string"),
    ("test package slice member", "internal/store/sqlitestore/code_review_store_test.go", "items[0].[[Id]] != fixedItem.Id",
     "internal/model/code_review.go", '[[Id]] string             `json:"id"` ReviewId string'),
)


def workspace_cases(root, stage):
    cases = [
        (label, locate(root / API, source), locate(root / target_path, target))
        for minimum, label, source, target_path, target in WORKSPACE_CASES
        if minimum <= stage
    ]
    if stage >= 4:
        cases.extend(
            (label, locate(root / source_path, source), locate(root / target_path, target))
            for label, source_path, source, target_path, target in CONTAINER_WORKSPACE_CASES
        )
    return cases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=executable)
    parser.add_argument("--stage", type=int, choices=(1, 2, 3, 4, 5), default=1)
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
                (source, sites[source], sites[target] if target else None)
                for stage, source, target in FIXTURE_CASES
                if stage <= args.stage
            ]
            if args.stage >= 3:
                cases.extend(builtin_cases(root, sites))
            overlays = [(sites["private-use"], sites["private-function"])]
            if args.stage >= 5:
                overlays.append((sites["cross-file-closure"], sites["cross-file-inline-final"]))
            success = exercise(args.candidate, root, cases, args.timeout, overlays)
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
