#!/usr/bin/env python3
"""Check document and workspace symbols against GoLand's recorded PSI and navigation."""

from __future__ import annotations

import argparse
from collections import Counter
from contextlib import contextmanager
from dataclasses import asdict, dataclass
from functools import cached_property
import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys

from check_goland_definitions import fixture_path
from compare_definitions import LspClient, executable, path_from_uri


DOCUMENT = "textDocument/documentSymbol"
WORKSPACE = "workspace/symbol"
KINDS = {
    "import": 2, "alias": 5, "definedType": 5, "method": 6, "interfaceMethod": 6,
    "field": 8, "embeddedField": 8, "interface": 11, "embeddedInterface": 11,
    "function": 12, "variable": 13, "constant": 14, "struct": 23,
}
FIXTURE_DETAILS = {
    "First": "= iota", "Second": "= iota", "Third": "= iota", "Fourth": "= iota",
    "Label": "string", "Title": "string", "Alias": "= int", "Count": "int",
    "Record": "struct", "Handler": "interface", "Location": "*clock.Location",
    "Box": {"struct", "Box[T]"}, "Port": "interface", "HandleRequest": "(int) error",
}
FUZZY_DIFFERENCES = {
    "localchat": {"camel", "subsequence", "method", "struct-field", "init"},
    "fixture": {"subsequence", "init", "generic-embed", "named-field", "qualified-field", "type-parameter"},
}
QUALIFIED_DIFFERENCES = {"package-type-method", "qualified-interface-method"}

Span = tuple[int, int, int, int]


def lsp_span(value) -> Span:
    span = tuple(value[point][unit] for point in ("start", "end") for unit in ("line", "character"))
    if any(type(n) is not int or n < 0 for n in span) or span[:2] > span[2:]:
        raise ValueError(f"invalid LSP UTF-16 range {span}")
    return span


def contains(outer: Span, inner: Span) -> bool:
    return outer[:2] <= inner[:2] <= inner[2:] <= outer[2:]


def unshift(span: Span, lines: int) -> Span:
    return span[0] - lines, span[1], span[2] - lines, span[3]


@dataclass(frozen=True, order=True)
class Target:
    path: str
    name: str
    kind: int
    span: Span
    container: str

    @property
    def identity(self):
        return self.path, self.name, self.kind, self.span


@dataclass(frozen=True)
class Declaration:
    target: Target
    display: str
    span: Span
    children: tuple[Declaration, ...]


class SourceText:
    def __init__(self, path):
        self.raw = path.read_bytes().decode("utf-8")

    @cached_property
    def lines(self):
        return [line.encode("utf-16-le") for line in self.raw.replace("\r\n", "\n").split("\n")]

    def slice(self, span):
        sl, sc, el, ec = span
        if not (0 <= sl <= el < len(self.lines) and
                0 <= 2 * sc <= len(self.lines[sl]) and 0 <= 2 * ec <= len(self.lines[el]) and
                span[:2] <= span[2:]):
            raise ValueError(f"range {span} is outside the source in UTF-16")
        if sl == el:
            return self.lines[sl][2 * sc:2 * ec].decode("utf-16-le")
        return "\n".join(line.decode("utf-16-le") for line in (
            self.lines[sl][2 * sc:], *self.lines[sl + 1:el], self.lines[el][:2 * ec],
        ))


def walk(rows):
    for row in rows:
        yield row
        yield from walk(row.get("children", ()))


def declarations(rows):
    for row in rows:
        yield row
        yield from declarations(row.children)


def receiver_base(spelling):
    return spelling.lstrip("*(").split("[", 1)[0].rstrip(")")


def subsequence(needle, haystack):
    letters = iter(haystack.lower())
    return all(ch in letters for ch in needle.lower())


def matches_query(query, target):
    qualifier, dot, name = query.lower().rpartition(".")
    if dot:
        container = target.container.lower()
        package, sep, owner = container.partition(".")
        if "." in qualifier:
            wanted_package, wanted_owner = qualifier.split(".", 1)
            if not (sep and wanted_package == package and subsequence(wanted_owner, owner)):
                return False
        elif not (subsequence(qualifier, owner) if sep else qualifier == package):
            return False
    return subsequence(name, target.name)


class SymbolCorpus:
    def __init__(self, workspace, fixture, *, check_revision=True):
        self.root = workspace.resolve(strict=True)
        self.data = json.loads(fixture.read_text(encoding="utf-8"))
        self.project = self.data["project"]
        if self.data["schemaVersion"] != 1 or self.data["scope"] != "Project":
            raise ValueError("expected schema version 1 and Project Scope")
        if check_revision and self.data["revision"]:
            revision = subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=self.root, encoding="utf-8",
            ).strip()
            if revision != self.data["revision"]:
                raise ValueError(f"GoLand captured revision {self.data['revision']}, found {revision}")
            if subprocess.check_output(
                ["git", "status", "--porcelain=v1", "--untracked-files=all", "--", "."],
                cwd=self.root, encoding="utf-8",
            ):
                raise ValueError(f"{self.root} must match the captured clean working tree")
        self.sources = {}
        self.references = {}
        self.outlines = {}
        self.queries = {}
        for row in self.data["outlines"]:
            relative = row["path"]
            source = self.source(relative)
            line_ending = "CRLF" if "\r\n" in source.raw else "LF"
            if line_ending != row["lineEnding"]:
                raise ValueError(f"{relative}: expected {row['lineEnding']}, found {line_ending}")
            for item in walk(row["declarations"]):
                if item.get("referenceIdentifierRange") is not None:
                    self.references[relative, tuple(item["navigationOffset"])] = item["referenceIdentifierRange"]
            outline = self.outline(row["declarations"])
            self.outlines[relative] = row, outline
        self.workspace_declarations = {
            self.go_target(row)
            for item in self.data["outlines"]
            for row in walk(item["declarations"])
            if row["kind"] in KINDS and row["kind"] != "import" and row["name"] != "_"
        }
        for row in self.data["queries"]:
            self.queries[row["id"]] = row, tuple(self.go_target(item) for item in row["results"])

    def path(self, relative):
        return fixture_path(relative, self.root)

    def source(self, relative):
        if relative not in self.sources:
            self.sources[relative] = SourceText(self.path(relative))
        return self.sources[relative]

    def go_identity(self, row):
        relative, name, kind = row["path"], row["name"], row["kind"]
        selection = row["identifierRange"] or row.get("referenceIdentifierRange")
        if selection is None and kind == "embeddedInterface":
            selection = self.references.get((relative, tuple(row["navigationOffset"])), row["psiRange"])
        if selection is None or self.source(relative).slice(selection) != name:
            raise ValueError(f"{relative}: GoLand {kind} {name!r} does not match UTF-16 {selection!r}")
        return relative, name, KINDS[kind], tuple(selection)

    def go_target(self, row):
        package, owner = row["packageName"], row["ownerType"]
        container = f"{package}.{receiver_base(owner)}" if owner else package
        return Target(*self.go_identity(row), container)

    def outline(self, rows, parent=None):
        entries = []
        for row in rows:
            kind, name = row["kind"], row["name"]
            if kind in {"constraint", "import"} or name == "_":
                continue
            relative, name, symbol_kind, selection = self.go_identity(row)
            receiver = row["ownerType"] if kind == "method" else None
            container = receiver_base(receiver) if receiver else parent or row["packageName"]
            display = f"({receiver}).{name}" if receiver else name
            decl, stmt = tuple(row["declarationRange"]), tuple(row["statementRange"])
            source = self.source(relative)
            grouped = re.match(r"(?:const|type|var)\s*\(", source.slice(stmt))
            span = decl if grouped else stmt
            if not contains(span, selection):
                raise ValueError(f"{relative}: GoLand declaration {span} excludes {name!r} at {selection}")
            entries.append(Declaration(
                Target(relative, name, symbol_kind, selection, container),
                display, span, self.outline(row["children"], name),
            ))
        return tuple(entries)

    def check_document(self, relative, result, *, hierarchical=True, lines=0):
        _, expected = self.outlines[relative]
        if not isinstance(result, list):
            raise ValueError(f"{relative}: expected document symbol list, got {result!r}")
        if hierarchical:
            self.check_tree(relative, expected, result, lines)
        else:
            wanted = tuple(sorted(
                (entry.target for entry in declarations(expected)), key=lambda target: target.span[:2],
            ))
            actual = tuple(self.lsp_target(row, relative, lines) for row in result)
            if actual != wanted:
                check_ordered(wanted, actual, relative)
        return sum(1 for _ in declarations(expected))

    def check_tree(self, relative, expected, rows, lines, parent=None):
        if not isinstance(rows, list) or len(rows) != len(expected):
            raise ValueError(f"{relative}: expected {[item.display for item in expected]}, got {rows!r}")
        source = self.source(relative)
        for entry, row in zip(expected, rows):
            target = entry.target
            label = f"{relative}:{target.span[0] + lines + 1}:{target.name}"
            if row["name"] != entry.display or row["kind"] != target.kind:
                raise ValueError(f"{label}: expected {(entry.display, target.kind)}, got {row!r}")
            selection = unshift(lsp_span(row["selectionRange"]), lines)
            span = unshift(lsp_span(row["range"]), lines)
            if selection != target.span or source.slice(selection) != target.name:
                raise ValueError(f"{label}: expected identifier {target.span}, got {selection}")
            if span == selection or not contains(span, selection) or (parent and not contains(parent, span)):
                raise ValueError(f"{label}: invalid enclosing range {span} for {selection} within {parent}")
            if entry.span == selection:
                before = source.slice((*span[:2], *selection[:2]))
                after = source.slice((*selection[2:], *span[2:]))
                padding = before + after
                if len(padding) != 1 or padding not in " \t\n;{}()":
                    raise ValueError(f"{label}: expected adjacent declaration boundary, got {span}")
            elif span != entry.span:
                raise ValueError(f"{label}: expected full declaration {entry.span}, got {span}")
            detail = row.get("detail")
            if detail is not None and (not isinstance(detail, str) or not detail.strip()):
                raise ValueError(f"{label}: invalid detail {detail!r}")
            if relative == "primary/outline.go":
                wanted = FIXTURE_DETAILS.get(target.name)
                if wanted is not None and detail not in (wanted if isinstance(wanted, set) else {wanted}):
                    raise ValueError(f"{label}: expected detail {wanted!r}, got {detail!r}")
            self.check_tree(relative, entry.children, row.get("children", []), lines, span)

    def lsp_target(self, row, edited_file=None, lines=0):
        location = row["location"]
        path = path_from_uri(location["uri"])
        if not path.is_relative_to(self.root):
            raise ValueError(f"symbol {row['name']!r} escaped Project Scope: {path}")
        relative = path.relative_to(self.root).as_posix()
        span = unshift(lsp_span(location["range"]), lines if edited_file == relative else 0)
        if self.source(relative).slice(span) != row["name"]:
            raise ValueError(f"{relative}: {span} does not select {row['name']!r}")
        return Target(relative, row["name"], row["kind"], span, row["containerName"])

    def check_workspace(self, case_id, result, *, edited_file=None, lines=0, baseline=None):
        if not isinstance(result, list):
            raise ValueError(f"{case_id}: expected workspace symbol list, got {result!r}")
        actual = tuple(self.lsp_target(row, edited_file, lines) for row in result)
        if len({(item.path, item.span) for item in actual}) != len(actual):
            raise ValueError(f"{case_id}: duplicate LSP physical targets")
        if baseline is not None:
            check_ordered(baseline, actual, case_id)
            return actual

        row, targets = self.queries[case_id]
        expected, result_set = set(targets), set(actual)
        missing, additional = expected - result_set, result_set - expected
        allowed_missing = set()
        if self.project == "fixture" and case_id == "alias":
            allowed_missing = {target for target in expected if target.kind == KINDS["import"]}
        if self.project == "localchat" and case_id == "struct-field":
            allowed_missing = {
                target for item, target in zip(row["results"], targets)
                if not subsequence(row["query"], target.name)
                and subsequence(row["query"], item["modelFullName"])
            }
        is_fuzzy = case_id in FUZZY_DIFFERENCES.get(self.project, set())
        is_qualified = case_id in QUALIFIED_DIFFERENCES
        allowed_additional = {
            target for target in additional
            if (is_fuzzy or is_qualified) and matches_query(row["query"], target)
        }
        if missing != allowed_missing or additional != allowed_additional:
            raise ValueError(
                f"{case_id} ({row['query']!r}): missing {sorted(missing - allowed_missing)!r}, "
                f"extra {sorted(additional - allowed_additional)!r}, "
                f"expected IDE-only {sorted(allowed_missing - missing)!r}",
            )
        if is_qualified or (self.project == "fixture" and is_fuzzy):
            wanted = {
                target for target in self.workspace_declarations if matches_query(row["query"], target)
            }
            if result_set != wanted:
                raise ValueError(f"{case_id}: missing {sorted(wanted - result_set)!r}, extra {sorted(result_set - wanted)!r}")
        if self.project == "localchat" and case_id == "subsequence":
            _, camel = self.queries["camel"]
            if not set(camel) <= result_set:
                raise ValueError(f"{case_id}: expected the two FileBackend factory targets")
        return actual

    def document_differences(self, relative, *, structure=None):
        row, outline = self.outlines[relative]
        structure = row["structure"] if structure is None else structure

        def go_tree(items, parents=()):
            for item in items:
                target = self.go_identity(item)
                yield target, parents
                yield from go_tree(item["children"], parents + (target,))

        def lsp_tree(items, parents=()):
            for item in items:
                target = item.target.identity
                yield target, parents
                yield from lsp_tree(item.children, parents + (target,))

        ide, lsp = tuple(go_tree(structure)), tuple(lsp_tree(outline))
        ide_set, lsp_set = Counter(target for target, _ in ide), Counter(target for target, _ in lsp)
        go_only = sorted((ide_set - lsp_set).elements())
        lsp_only = sorted((lsp_set - ide_set).elements())
        reasons = []
        if go_only or lsp_only:
            if any(target[1] == "_" for target in go_only):
                reasons.append("blank identifier")
            if any(target[0] != relative for target in go_only):
                reasons.append("cross-file receiver in Structure View")
            if any(target[1] != "_" and target[0] == relative for target in [*go_only, *lsp_only]):
                reasons.append("receiver placement and Structure View inclusion")
        if Counter(ide) != Counter(lsp) and ide_set == lsp_set:
            reasons.append("receiver hierarchy")
        shared = Counter(ide) & Counter(lsp)
        if [item for item in ide if shared[item]] != [item for item in lsp if shared[item]]:
            reasons.append("natural kind order versus source order")
        if any(entry.span == entry.target.span for entry in declarations(outline)):
            reasons.append("strict declaration range for a bare embedding")
        return {
            "reasons": reasons,
            "goland_only": go_only,
            "rs_gopls_only": lsp_only,
            "goland_nodes": len(ide),
            "rs_gopls_nodes": len(lsp),
        }

    def workspace_differences(self, case_id, actual):
        row, ide = self.queries[case_id]
        added, missing = set(actual) - set(ide), set(ide) - set(actual)
        reasons = []
        if case_id == "alias" and missing:
            reasons.append("explicit import alias")
        if case_id == "struct-field" and missing:
            reasons.append("GoLand's unqualified search also matched model full names")
        if added:
            if case_id in QUALIFIED_DIFFERENCES:
                reasons.append("package.type.member qualification")
            elif case_id in {"named-field", "qualified-field"}:
                reasons.append("single-letter member search")
            else:
                reasons.append("general case-insensitive subsequence search")
        return {
            "query": row["query"],
            "reasons": reasons,
            "goland_only": [asdict(item) for item in sorted(missing)],
            "rs_gopls_only": [asdict(item) for item in sorted(added)],
            "goland_rows": len(ide),
            "goland_duplicate_rows": len(ide) - len(set(ide)),
            "common_physical_targets": len(set(ide) & set(actual)),
            "same_physical_order": tuple(dict.fromkeys(ide)) == actual,
        }


def check_ordered(expected, actual, label):
    if expected != actual:
        missing = list((Counter(expected) - Counter(actual)).elements())
        extra = list((Counter(actual) - Counter(expected)).elements())
        raise ValueError(f"{label}: missing {missing!r}, extra {extra!r}"
                         + ("" if missing or extra else "; response order changed"))


def go_environment(corpus, timeout):
    modules = corpus.data["sdk"]["modules"]
    if not modules or len({(m["sdkVersion"], m["sdkHome"]) for m in modules}) != 1:
        raise ValueError("expected a single Go SDK in the GoLand capture")
    module = modules[0]
    sdk = Path(module["sdkHome"])
    env = os.environ.copy()
    if not (sdk / "bin" / "go.exe").is_file():
        toolchain = f"go{module['sdkVersion']}"
        sdk = Path(subprocess.check_output(
            ["go", "env", "GOROOT"], cwd=corpus.root, encoding="utf-8", timeout=timeout,
            env={**env, "GOTOOLCHAIN": toolchain, "GOPROXY": "off"},
        ).strip())
    path_key = next((key for key in env if key.upper() == "PATH"), "PATH")
    env[path_key] = str(sdk / "bin") + os.pathsep + env.get(path_key, "")
    work = corpus.data["sdk"]["goWork"]
    env.update(
        GOROOT=str(sdk), GOTOOLCHAIN="local", GOWORK=work if work == "off" else str(corpus.path(work)),
        GOPROXY="off", GOFLAGS="", GOOS="windows", GOARCH="amd64", CGO_ENABLED="1", GO111MODULE="on",
    )
    version = subprocess.check_output(
        [str(sdk / "bin" / "go.exe"), "version"], cwd=corpus.root, env=env,
        encoding="utf-8", timeout=timeout,
    ).strip()
    if version != f"go version go{module['sdkVersion']} windows/amd64":
        raise ValueError(f"unexpected Go SDK: {version}")
    return env, version


@contextmanager
def symbol_session(server, corpus, timeout, environment, *, hierarchical):
    client = LspClient([server], corpus.root, timeout, environment=environment)
    try:
        initialize_symbols(client, corpus, hierarchical)
        yield client
        client.shutdown()
    finally:
        client.close()


def initialize_symbols(client, corpus, hierarchical):
    capabilities = client.initialize(hierarchical_document_symbols=hierarchical)
    for provider in ("documentSymbolProvider", "workspaceSymbolProvider"):
        if capabilities.get(provider) in (None, False):
            raise ValueError(f"server did not advertise {provider}")
    for relative in corpus.data["sdk"]["openFiles"]:
        client.open_document(corpus.path(relative), corpus.source(relative).raw)


def request_document(client, path):
    return client.request(DOCUMENT, textDocument={"uri": path.as_uri()})


def replay(args, corpus, env):
    outline_results, workspace_results, baselines = {}, {}, {}
    for hierarchical in (True, False):
        with symbol_session(args.server, corpus, args.timeout, env, hierarchical=hierarchical) as client:
            for relative in corpus.outlines:
                count = corpus.check_document(
                    relative, request_document(client, corpus.path(relative)), hierarchical=hierarchical,
                )
                outline_results.setdefault(relative, corpus.document_differences(relative))
                print(f"PASS {'nested' if hierarchical else 'flat'} {relative}: {count} symbols")
            for case_id, (row, _) in corpus.queries.items():
                actual = corpus.check_workspace(
                    case_id, client.request(WORKSPACE, query=row["query"]), baseline=baselines.get(case_id),
                )
                if hierarchical:
                    baselines[case_id] = actual
                    workspace_results[case_id] = corpus.workspace_differences(case_id, actual)
    for cases in (outline_results, workspace_results):
        for case_id, diff in cases.items():
            if diff["reasons"]:
                print(f"DIFF {case_id}: {', '.join(diff['reasons'])} "
                      f"(GoLand-only {len(diff['goland_only'])}, rs-gopls-only {len(diff['rs_gopls_only'])})")
    duplicate_rows = sum(result["goland_duplicate_rows"] for result in workspace_results.values())
    common_queries = sum(not result["reasons"] for result in workspace_results.values())
    print(f"PASS {len(outline_results)} outlines in each mode, "
          f"{len(workspace_results)} workspace queries in both sessions "
          f"({common_queries} identical physical sets, {duplicate_rows} duplicate GoLand rows)")
    return {"outlines": outline_results, "workspace": workspace_results}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=executable)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be positive and finite")
    try:
        corpus = SymbolCorpus(args.workspace, args.fixture)
        env, version = go_environment(corpus, args.timeout)
        print(f"{corpus.project}: GoLand {corpus.data['build']}, {version}")
        result = replay(args, corpus, env)
        if args.json:
            args.json.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    except (KeyError, OSError, RuntimeError, ValueError, subprocess.SubprocessError) as exc:
        print(f"FAIL {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
