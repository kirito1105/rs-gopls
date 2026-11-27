#!/usr/bin/env python3
"""Replay GoLand BASIC completion fixtures against rs-gopls over stdio."""

import argparse
from collections import Counter, defaultdict
from contextlib import contextmanager
from dataclasses import dataclass
import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys
import time

from check_goland_definitions import fixture_path, utf16_offset
from check_goland_symbols import WORKSPACE, go_environment
from lsp_harness import (
    WORKSPACE_ENVIRONMENT, LspClient, acknowledge_file_watches, executable, position_at,
)


COMPLETION = "textDocument/completion"
RESOLVE = "completionItem/resolve"
RESOLVE_PROPERTIES = ("detail", "documentation", "additionalTextEdits")
START = (0, 0)
METADATA = ("schemaVersion", "project", "root", "revision", "build", "sdk", "completionType",
            "invocationCount", "editPrefix")
TYPE_KINDS = {7, 8, 13, 22}
GOLAND_KINDS = {
    "GoFunctionDeclarationImpl": {3},
    "GoMethodDeclarationImpl": {2},
    "GoMethodSpecImpl": {2},
    "GoFieldDefinitionImpl": {5},
    "GoAnonymousFieldDefinitionImpl": {5},
    "GoTypeSpecImpl": TYPE_KINDS,
    "GoTypeParamDefinitionImpl": {25},
    "GoConstDefinitionImpl": {21},
    "GoVarDefinitionImpl": {6},
    "GoParamDefinitionImpl": {6},
    "GoReceiverImpl": {6},
    "GoLabelDefinitionImpl": {18},
}
AMBIGUOUS_SELECTORS = {
    "selector-eof", "selector-newline", "selector-before-paren", "selector-next-statement",
    "selector-direct-promoted", "selector-pointer", "selector-type-method",
    "selector-pointer-method-expression", "selector-nonaddressable", "selector-ambiguous-field",
    "selector-ambiguous-method",
}
LOCAL_SHADOWS = {
    "shadow-local-package", "shadow-local-package-bare", "shadow-local-import",
    "shadow-local-import-bare", "shadow-local-import-package-error",
}
SELECTOR_BASE_CASES = {"local-receiver", "shadow-package", "shadow-local-package",
                       "shadow-import", "shadow-local-import"}
RECEIVER_EXPANSIONS = {
    "scope-all": "receiver", "scope-receiver": "receiver", "scope-result": "receiver",
    "local-inferred": "u", "local-receiver": "u",
}
OPEN_IMPORT_ANCHORS = {
    "import-empty-quote": ("fmt", "example.test/completion/pkg/helper"),
    "import-workspace": ("example.test/completion/pkg/helper",),
    "local-edit-import-path": ("github.com/anthropic/chatting/mm/server/internal/config",),
}
SINGLE_IMPORT_CASES = {"auto-marshal", "auto-single-comment", "auto-versioned", "auto-go-prefix",
                       "auto-odd-path", "auto-vendor"}


def read_json(path):
    data = json.loads(path.read_text(encoding="utf-8-sig"))
    if data.get("root") == "$WORKSPACE" or "completionType" not in data:
        return data
    data["root"] = "$WORKSPACE"
    if data.get("revision"):
        data["project"] = "external"
    cache_roots = set()
    for module in data["sdk"]["modules"]:
        sdk = module["sdkHome"].replace("\\", "/")
        cache_root, marker, _ = sdk.rpartition("/golang.org/toolchain@")
        if marker:
            cache_roots.add(cache_root + "/")
        module["sdkHome"] = "$GOROOT"

    def relocate(node):
        if isinstance(node, list):
            for child in node:
                relocate(child)
        elif isinstance(node, dict):
            source = node.get("sourcePath")
            if source:
                for prefix in cache_roots:
                    if source.startswith(prefix):
                        node["sourcePath"] = "$GOMODCACHE/" + source[len(prefix):]
                        break
            for child in node.values():
                relocate(child)

    relocate(data["cases"])
    return data


def write_report(path, report):
    path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def position(point):
    return dict(zip(("line", "character"), point, strict=True))


def lsp_range(span):
    return {"start": position(span[:2]), "end": position(span[2:])}


def range_span(value):
    return tuple(value[end][key] for end in ("start", "end") for key in ("line", "character"))


def shifted(span, lines):
    return span[0] + lines, span[1], span[2] + lines, span[3]


def lf(text):
    return text.replace("\r\n", "\n")


class DocumentText:
    def __init__(self, text):
        self.text = text
        self.eol = "\r\n" if "\r\n" in text else "\n"
        self.lines = text.split("\n")
        self.starts = [0]
        for line in self.lines[:-1]:
            self.starts.append(self.starts[-1] + len(line) + 1)
        self.lines = [line.removesuffix("\r") for line in self.lines]
        self.offsets = {}

    def offset(self, point):
        line, column = point
        if type(line) is not int or type(column) is not int or not 0 <= line < len(self.lines):
            raise ValueError(f"invalid UTF-16 position {point!r}")
        if point not in self.offsets:
            self.offsets[point] = self.starts[line] + utf16_offset(self.lines[line], 0, column)
        return self.offsets[point]

    def offsets_for(self, span):
        start, end = self.offset(span[:2]), self.offset(span[2:])
        if start > end:
            raise ValueError(f"reversed UTF-16 range {span!r}")
        return start, end

    def apply(self, edits):
        offsets = sorted((*self.offsets_for(span), replacement) for span, replacement in edits)
        for (old_start, old_end, _), (start, _, _) in zip(offsets, offsets[1:]):
            if start < old_end or start == old_start:
                raise ValueError(f"overlapping text edits at offsets {old_start}:{old_end} and {start}")
        result = self.text
        for start, end, replacement in reversed(offsets):
            result = result[:start] + replacement + result[end:]
        return result

    def capture_text(self, text):
        return lf(text).replace("\n", self.eol)


def completion_params(path, point, trigger_character=None):
    context = {"triggerKind": 1}
    if trigger_character is not None:
        context = {"triggerKind": 2, "triggerCharacter": trigger_character}
    return {"textDocument": {"uri": path.as_uri()}, "position": position(point), "context": context}


def warm_package_graph(client):
    if client.request(WORKSPACE, query="rsGoplsCompletionGraphReadyProbe") != []:
        raise RuntimeError("the package graph readiness query unexpectedly matched symbols")


@contextmanager
def completion_session(server, root, timeout, *, environment=None, watch_files=False, full=True,
                       resolve_properties=RESOLVE_PROPERTIES):
    client = LspClient([server], root, timeout, environment={
        **os.environ, **WORKSPACE_ENVIRONMENT, **(environment or {}),
    })
    try:
        workspace = {"workspaceFolders": True}
        if watch_files:
            workspace["didChangeWatchedFiles"] = {"dynamicRegistration": True}
        capabilities = {"general": {"positionEncodings": ["utf-16"]}, "workspace": workspace}
        if full:
            capabilities["textDocument"] = {"completion": {
                "contextSupport": True,
                "completionItem": {
                    "snippetSupport": True, "labelDetailsSupport": True, "insertReplaceSupport": True,
                    "deprecatedSupport": True, "tagSupport": {"valueSet": [1]},
                    "documentationFormat": ["markdown", "plaintext"],
                    "resolveSupport": {"properties": list(resolve_properties)},
                },
            }}
        result = client.request(
            "initialize", processId=os.getpid(), rootUri=root.as_uri(), workspaceFolders=client.folders,
            initializationOptions={"diagnosticsTrigger": "Save"}, capabilities=capabilities,
        )["capabilities"]
        if result.get("positionEncoding", "utf-16").lower() != "utf-16":
            raise RuntimeError("server did not negotiate UTF-16 positions")
        client.notify("initialized")
        if watch_files:
            acknowledge_file_watches(client)
        yield client, result
        client.shutdown()
    except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
        log = client.stderr_tail()
        raise RuntimeError(f"{root}: {exc}" + (f"\n{log}" if log else "")) from exc
    finally:
        client.close()


def completion_items(response):
    if response is None:
        return []
    if isinstance(response, dict):
        if response.get("isIncomplete") is not False or response.get("itemDefaults"):
            raise ValueError("expected a complete list with per-item edits for explicit completion")
        response = response.get("items")
    if not isinstance(response, list) or any(not isinstance(item, dict) for item in response):
        raise ValueError("expected a CompletionList, CompletionItem array, or null")
    return response


def primary_edit(item, source, point):
    edit = item["textEdit"]
    if not isinstance(edit["newText"], str):
        raise ValueError("textEdit.newText must be a string")
    if "range" in edit:
        if "insert" in edit or "replace" in edit:
            raise ValueError("textEdit mixes TextEdit and InsertReplaceEdit")
        insert, replace = None, range_span(edit["range"])
    else:
        insert, replace = range_span(edit["insert"]), range_span(edit["replace"])
        source.offsets_for(insert)
        if insert[:2] != replace[:2] or insert[2:] != point:
            raise ValueError("InsertReplaceEdit.insert must share the replacement start and end at the cursor")
    source.offsets_for(replace)
    if replace[0] != point[0] or replace[2] != point[0] or not replace[1] <= point[1] <= replace[3]:
        raise ValueError(f"replacement {replace!r} must be on the cursor line and contain {point!r}")
    return edit["newText"], insert, replace


# Track A emits tab stops, possibly with a default. Expand them before testing
# accepted text so that snippet syntax cannot be mistaken for source edits.
SNIPPET_TOKEN = re.compile(r"\\([$}\\])|\$(\d+)|\$\{(\d+)(?::([^{}]*))?\}|\$")


def snippet_text(text):
    def expand(match):
        escaped, stop, braced_stop, default = match.groups()
        if escaped is not None:
            return escaped
        if stop is not None or braced_stop is not None:
            return snippet_text(default) if default is not None else ""
        raise ValueError(f"unsupported completion snippet {text!r}")

    return SNIPPET_TOKEN.sub(expand, text)


def apply_completion(source, point, item, *, replace=False):
    new_text, insert, replacement = primary_edit(item, source, point)
    text_format = item.get("insertTextFormat", 1)
    if text_format == 2:
        new_text = snippet_text(new_text)
    elif text_format != 1:
        raise ValueError(f"invalid insertTextFormat {text_format!r}")
    extras = [(range_span(edit["range"]), edit["newText"]) for edit in item.get("additionalTextEdits", ())]
    chosen = replacement if replace or insert is None else insert
    if chosen != replacement and extras:
        source.apply([(replacement, new_text), *extras])
    return source.apply([(chosen, new_text), *extras])


@dataclass
class Query:
    row: dict
    path: Path
    source: DocumentText
    edits: list
    edited_source: DocumentText
    point: tuple

    @property
    def id(self):
        return self.row["id"]


class CompletionCorpus:
    def __init__(self, workspace, fixture):
        self.root = workspace.resolve(strict=True)
        self.fixture = fixture.resolve(strict=True)
        self.data = read_json(self.fixture)
        if (self.data["schemaVersion"], self.data["completionType"], self.data["invocationCount"]) != (1, "BASIC", 1):
            raise ValueError("expected GoLand BASIC invocation 1, fixture schema version 1")
        if revision := self.data["revision"]:
            actual = self.git("rev-parse", "HEAD").strip()
            if actual != revision or self.git("status", "--porcelain=v1", "--untracked-files=all", "--", "."):
                raise ValueError(f"{self.root} must match the clean GoLand capture at {revision}, found {actual}")
        self.edit_prefix = self.data["editPrefix"]
        if not self.edit_prefix.endswith("\n") or "\r" in self.edit_prefix:
            raise ValueError("expected a benchmark edit consisting of complete LF lines")
        self.edit_lines = self.edit_prefix.count("\n")
        self.sources = {}
        self.origins = {}
        self.active_packages = set()
        self.queries = {}
        for row in self.data["cases"]:
            query = self.query(row)
            if query.id in self.queries:
                raise ValueError(f"duplicate completion query {query.id!r}")
            self.queries[query.id] = query
        if not self.queries:
            raise ValueError("expected a nonempty completion fixture")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, encoding="utf-8")

    def load_active_packages(self, environment, timeout):
        self.sdk = Path(environment["GOROOT"])
        self.module_cache = Path(subprocess.check_output(
            [str(self.sdk / "bin" / "go.exe"), "env", "GOMODCACHE"], cwd=self.root,
            env=environment, encoding="utf-8", timeout=timeout,
        ).strip())
        self.open_files = [
            self.origin(path) if path.startswith(("$GOROOT/", "$GOMODCACHE/")) else self.path(path)
            for path in self.data["sdk"]["openFiles"]
        ]
        listing = subprocess.check_output(
            [str(self.sdk / "bin" / "go.exe"), "list", "-e", "-deps", "-test", "./..."],
            cwd=self.root, env=environment, encoding="utf-8", timeout=timeout,
        )
        self.active_packages = {line.split(" [", 1)[0] for line in listing.splitlines()}

    def path(self, relative):
        return fixture_path(relative, self.root)

    def source(self, path):
        if path not in self.sources:
            self.sources[path] = DocumentText(path.read_bytes().decode("utf-8"))
        return self.sources[path]

    def query(self, row):
        path = self.path(row["path"])
        source = self.source(path)
        ending = "CRLF" if source.eol == "\r\n" else "LF"
        if row["lineEnding"] != ending:
            raise ValueError(f"{row['id']}: expected {row['lineEnding']} line endings in {path}, found {ending}")
        edits = []
        for edit in row.get("edits", ()):
            span = tuple(edit["range"])
            start, end = source.offsets_for(span)
            if lf(source.text[start:end]) != edit["expectedText"]:
                raise ValueError(f"{row['id']}: expected {edit['expectedText']!r} at {span}")
            replacement = source.capture_text(edit["text"])
            edits.append((span, replacement))
            source = DocumentText(source.apply([(span, replacement)]))
        point = row["line"], row["character"]
        source.offset(point)
        if source.lines[point[0]] != row["lineText"]:
            raise ValueError(f"{row['id']}: source line differs from the captured caret line")
        source.offset(tuple(row["identifierEnd"]))
        for index, candidate in enumerate(row["candidates"]):
            for key in ("prefixRange", "replacementRange"):
                source.offsets_for(tuple(candidate[key]))
            if candidate["order"] != index or tuple(candidate["prefixRange"][2:]) != point:
                raise ValueError(f"{row['id']}: invalid captured candidate order or range")
        if "accept" in row:
            acceptance = row["acceptance"]
            matches = [c["order"] for c in row["candidates"] if accept_target(c, row["accept"])]
            if (acceptance["target"] != row["accept"] or matches != acceptance["matchingOrders"] or
                    acceptance["status"] != {0: "missing", 1: "accepted"}.get(len(matches), "ambiguous")):
                raise ValueError(f"{row['id']}: acceptance target differs from captured candidates")
            if acceptance["status"] == "accepted":
                DocumentText(acceptance["documentText"]).offset(tuple(acceptance["caret"]))
        post_edit = DocumentText(source.capture_text(self.edit_prefix) + source.text)
        return Query(row, path, source, edits, post_edit, point)

    def origin(self, path):
        if path is None:
            return None
        if path not in self.origins:
            if path.startswith("$GOROOT/"):
                self.origins[path] = self.sdk / path.removeprefix("$GOROOT/")
            elif path.startswith("$GOMODCACHE/"):
                self.origins[path] = self.module_cache / path.removeprefix("$GOMODCACHE/")
            else:
                # Windows verbatim paths compare against regular capture paths.
                self.origins[path] = self.root / Path(path.removeprefix("\\\\?\\"))
        return self.origins[path]


def accept_target(candidate, target):
    return all(candidate.get(key) == value for key, value in target.items() if key != "select")


class OpenBuffers:
    def __init__(self, client, corpus):
        self.client, self.corpus = client, corpus
        self.versions = {}
        for path in corpus.open_files:
            self.open(path)

    def open(self, path):
        self.client.open_document(path, self.corpus.source(path).text)
        self.versions[path] = 1

    def edit(self, path, span, replacement):
        self.versions[path] += 1
        self.client.change_document_range(path, self.versions[path], text=replacement, **lsp_range(span))

    @contextmanager
    def replay(self, query):
        opened_here = query.path not in self.versions
        if opened_here:
            self.open(query.path)
        try:
            for span, replacement in query.edits:
                self.edit(query.path, span, replacement)
            yield
        finally:
            if query.edits:
                self.versions[query.path] += 1
                self.client.change_document(query.path, self.versions[query.path], self.corpus.source(query.path).text)
            if opened_here:
                self.client.close_document(query.path)
                del self.versions[query.path]

    @contextmanager
    def benchmark_edit(self, query):
        self.edit(query.path, (*START, *START), query.source.capture_text(self.corpus.edit_prefix))
        try:
            yield
        finally:
            self.edit(query.path, (*START, self.corpus.edit_lines, 0), "")


def goland_kinds(query, candidate):
    if candidate["kind"] == "String":
        return {9} if query.row["kind"] == "package" else {14}
    if candidate["kind"] == "PsiDirectoryImpl":
        return {9, 19} if query.row["kind"] == "importPath" else {9}
    if candidate["lookupString"] == "nil" and candidate["sourcePackage"] == "builtin":
        return {6, 21}
    return GOLAND_KINDS[candidate["kind"]]


def excluded_directory(path):
    return any(part == "testdata" or part.startswith((".", "_")) for part in path.split("/"))


def goland_omission(query, candidate):
    name, kind = candidate["lookupString"], candidate["kind"]
    if kind in {"PostfixTemplateLookupElement", "LiveTemplateLookupElementImpl"}:
        return "ide_template"
    if kind == "String":
        if query.id == "line-comment" and name == "Moon":
            return "comment_word_completion"
        if name in {"填充所有字段…", "填充选定字段…", "填充所选字段…"} or (
            query.id == "keyword-file" and candidate["typeText"] in {"Method", "Implement Interface..."}
        ):
            return "ide_code_generation"
        if name in {"array", "slice"} or (name in {"func", "map", "struct"} and candidate["tailText"]):
            return "ide_expression_template"
        if name == "map" and query.row["kind"] not in {"package", "type"} and query.id != "parameter-type-recovery":
            return "expression_type_keyword"
    if query.id == "selector-double-pointer":
        return "invalid_double_pointer_selector"
    if query.id in AMBIGUOUS_SELECTORS and name in {"Shared", "Collision"}:
        return "ambiguous_promoted_member"
    if query.id in {"selector-type-method", "selector-nonaddressable"} and name in {"LeftPointer", "PointerMethod"}:
        return "pointer_method_requires_addressability_or_pointer_method_set"
    if query.id in SELECTOR_BASE_CASES and "." in name and candidate["priority"] in {5, 35}:
        return "existing_selector_suffix_filters_auto_imported_members"
    receiver = RECEIVER_EXPANSIONS.get(query.id)
    if receiver and name.startswith(receiver + ".") and candidate["sourcePath"] == query.row["path"]:
        return "ide_unqualified_receiver_expansion"
    if query.id in {"literal-mid-key", "local-edit-literal", "local-literal"} and (
        kind != "GoFieldDefinitionImpl" or query.id == "local-literal" and name == "Routes"
    ):
        return "keyed_literal_context_filters_values_and_used_keys"
    if excluded_directory(candidate["sourcePackage"] or name) and (
        query.row["kind"] == "importPath" or candidate["sourcePackage"]
    ):
        return "excluded_go_package_directory"
    if query.id == "auto-vendor" and (candidate["sourcePath"] or "").startswith(".deps/vendordep/"):
        return "vendor_replace_duplicate"
    if query.id == "label-continue" and name == "ExitPoint":
        return "continue_requires_an_enclosing_loop_label"
    return None


def missing_difference(corpus, query, expected):
    name, package, path = (expected[key] for key in ("lookupString", "sourcePackage", "sourcePath"))
    if path and Path(path).is_absolute() and package and package not in corpus.active_packages:
        return "dependency_outside_active_package_graph"
    if package == "strings" and query.row["kind"] == "shadow" and name.startswith(("append.", "append2.")):
        return "ide_alias_index_or_shadowed_import"
    if query.id == "scope-blank-import" and package == "encoding/json" and name.startswith("j."):
        return "ide_alias_index_or_shadowed_import"
    return None


def extra_difference(corpus, query, item):
    if query.id == "literal-open-brace" and item.origin == query.path:
        if item.name in {"labels", "outsideConst", "sequence", "suppression"}:
            return "restored_scope_after_unclosed_literal"
        if (item.name in {"A", "Field"} and item.item.get("kind") == 5
                and snippet_text(item.new_text) == f"{item.name}: "):
            return "restored_field_keys_after_unclosed_literal"
    if (query.id == "keyword-range" and item.item.get("kind") == 21
            and item.path == "debug/elf" and item.origin == corpus.origin("$GOROOT/src/debug/elf/elf.go")
            and item.name.startswith(("elf.R_AARCH64_P32_", "elf.R_AARCH64_TLS"))
            and item.new_text == item.name):
        return "additional_matching_sdk_constants"
    return None


def goland_result(query, prefix):
    row = query.row
    result = {key: row[key] for key in ("identifierEnd", "selectedIndex", "candidates")}
    if "acceptance" in row:
        result["acceptance"] = row["acceptance"]
    if prefix:
        lines = prefix.count("\n")
        result["identifierEnd"] = [row["identifierEnd"][0] + lines, row["identifierEnd"][1]]
        result["candidates"] = [
            {**item, **{key: shifted(item[key], lines) for key in ("prefixRange", "replacementRange")}}
            for item in row["candidates"]
        ]
        acceptance = result.get("acceptance")
        if acceptance and acceptance["status"] == "accepted":
            line, column = acceptance["caret"]
            result["acceptance"] = {**acceptance, "documentText": prefix + acceptance["documentText"],
                                    "caret": [line + lines, column]}
    return result


IDENTIFIER = re.compile(r"[^\W\d]\w*")
AFTER_QUALIFIER = re.compile(r"([^\W\d]\w*)\.$")


def spelling(item, new_text):
    label = item["label"]
    if not isinstance(label, str) or not label:
        raise ValueError("completion item has no label")
    qualifier, sep, member = new_text.partition(".")
    if sep and IDENTIFIER.fullmatch(qualifier) and IDENTIFIER.fullmatch(label):
        word = IDENTIFIER.match(member)
        if word and word[0] == label:
            return f"{qualifier}.{label}"
    return label


@dataclass
class Candidate:
    item: dict
    name: str
    new_text: str
    insert: tuple | None
    replacement: tuple
    path: str | None
    target: dict
    origin: Path | None

    @classmethod
    def from_item(cls, corpus, query, item, source, point):
        new_text, insert, replacement = primary_edit(item, source, point)
        if item.get("additionalTextEdits"):
            apply_completion(source, point, item, replace=True)
        data = item.get("data")
        data = data if isinstance(data, dict) else {}
        target = data.get("target") or {}
        pending = data.get("pending_import") or {}
        path = (item.get("labelDetails") or {}).get("description") or pending.get("path")
        name = spelling(item, new_text)
        if query.row["kind"] == "autoImport" and pending and IDENTIFIER.fullmatch(name):
            # An auto-import edit may leave the existing qualifier in place.
            offset = source.offset(replacement[:2])
            qualifier = AFTER_QUALIFIER.search(source.text[source.starts[point[0]]:offset])
            if qualifier:
                name = f"{qualifier[1]}.{name}"
        return cls(item, name, new_text, insert, replacement, path, target, corpus.origin(target.get("file")))


def declared_append(query, source, candidate, *, local):
    if candidate.item.get("kind") != 6 or candidate.origin != query.path:
        return False
    pattern = r"(?m)^[ \t]*(append)\s*:=" if local else r"(?m)^var\s+(append)\s+PackageAppend\b"
    match = re.search(pattern, source.text)
    return bool(match and candidate.target.get("range", {}).get("start") == position_at(source.text, match.start(1)))


def alternate_acceptance(query, expected):
    if query.id in SINGLE_IMPORT_CASES:
        comment = " // Keep this inline comment with fmt."
        return ("preserved_single_import_literal_and_comment", expected.replace(
            '\tformatting "fmt"\n', "\tformatting `fmt`" + comment + "\n", 1,
        ).replace(")" + comment + "\n", ")\n", 1))
    if query.id == "auto-cgo":
        return ("preserved_cgo_comment", expected.replace(
            'import "C"\nimport "encoding/json" // Keep the cgo declaration separate.',
            'import "C" // Keep the cgo declaration separate.\n\nimport "encoding/json"', 1,
        ))
    if query.id == "literal-promoted":
        return ("nested_promoted_literal_key_for_go_1_25", expected.replace(
            "embedded.Outer{Basic: 1, LeftOnly: }", "embedded.Outer{Basic: 1, Left: embedded.Left{LeftOnly: }}", 1,
        ))
    return None


@dataclass
class Comparison:
    goland: dict
    rs_gopls: dict
    failures: list
    intentional_divergences: list
    completion_ms: float | None
    resolve_ms: float | None

    @property
    def matched(self):
        return not self.failures and not self.intentional_divergences


def timed_request(client, method, params):
    started = time.perf_counter_ns()
    try:
        response = client.wait_response(client.start_request(method, **params))
    except (OSError, RuntimeError) as exc:
        response = {"transport_error": str(exc)}
    return response, (time.perf_counter_ns() - started) / 1_000_000


class ExpectedCompletion:
    def __init__(self, corpus, query, *, after_edit=False):
        self.corpus, self.query = corpus, query
        self.source = query.edited_source if after_edit else query.source
        lines = corpus.edit_lines if after_edit else 0
        self.point = query.point[0] + lines, query.point[1]
        self.params = completion_params(query.path, self.point)
        self.goland = goland_result(query, corpus.edit_prefix if after_edit else "")
        self.expected, self.omissions = [], []
        for candidate in self.goland["candidates"]:
            if reason := goland_omission(query, candidate):
                self.omissions.append({"category": reason, "goland": candidate})
            else:
                self.expected.append(candidate)

    def request(self, client):
        actual, failures, divergences = {}, [], []
        response, elapsed = timed_request(client, COMPLETION, self.params)
        comparison = Comparison(self.goland, actual, failures, divergences, elapsed, None)
        if "result" not in response:
            failures.append({"category": "completion_protocol_error", "response": response})
            return comparison
        actual["completion"] = response["result"]
        try:
            items = completion_items(actual["completion"])
            candidates = [Candidate.from_item(self.corpus, self.query, item, self.source, self.point) for item in items]
            keys = [candidate.item["sortText"] for candidate in candidates]
            if keys != sorted(set(keys)):
                failures.append({"category": "unsorted_or_duplicate_sort_text", "rs_gopls": keys})
            pairs = self.compare(candidates, failures, divergences)
        except (AttributeError, KeyError, TypeError, ValueError) as exc:
            failures.append({"category": "invalid_completion_result_or_edits", "message": str(exc)})
            return comparison
        if "acceptance" in self.goland:
            self.accept(client, candidates, pairs, comparison)
        return comparison

    def compare(self, candidates, failures, divergences):
        actual_by_name = defaultdict(list)
        for index, candidate in enumerate(candidates):
            actual_by_name[candidate.name].append(index)
        used, pairs = set(), {}
        for expected in self.expected:
            name = expected["lookupString"]
            indexes = actual_by_name[name]
            if not indexes and self.query.id == "local-edit-type" and (
                expected["sourcePackage"] == "os/user" and name.startswith("user2.")
            ):
                indexes = actual_by_name["user." + name.removeprefix("user2.")]
            if not indexes:
                row = {"goland": expected, "rs_gopls": None}
                if reason := missing_difference(self.corpus, self.query, expected):
                    divergences.append({"category": reason, **row})
                else:
                    failures.append({"category": "missing_candidate", **row})
                continue
            origin = self.corpus.origin(expected["sourcePath"])
            package = expected["sourcePackage"]
            index = min(indexes, key=lambda i: (candidates[i].origin != origin, candidates[i].path != package))
            indexes.remove(index)
            candidate = candidates[index]
            used.add(index)
            pairs[expected["order"]] = index
            self.compare_candidate(expected, candidate, origin, failures, divergences)
            if candidate.name != name:
                pending = (candidate.item.get("data") or {}).get("pending_import")
                row = {"goland": expected, "rs_gopls": candidate.item}
                if pending == {"path": "os/user", "alias": None} and candidate.origin == origin:
                    divergences.append({"category": "alias_matches_declaration_scope", **row})
                else:
                    failures.append({"category": "wrong_import_alias", **row})
        extra_names = {name for name, indexes in actual_by_name.items() if indexes}
        for omitted in self.omissions:
            name = omitted["goland"]["lookupString"]
            if name not in extra_names:
                divergences.append({**omitted, "rs_gopls": None})
        extras = [candidate for index, candidate in enumerate(candidates) if index not in used]
        if self.query.id in OPEN_IMPORT_ANCHORS and not self.goland["candidates"]:
            self.compare_open_import(extras, failures, divergences)
        else:
            for item in extras:
                if reason := extra_difference(self.corpus, self.query, item):
                    divergences.append({"category": reason, "goland": None, "rs_gopls": item.item})
                else:
                    failures.append({"category": "unexpected_candidate", "rs_gopls": item.item})
        if list(pairs.values()) != sorted(pairs.values()):
            divergences.append({
                "category": "snapshot_based_sorting",
                "goland": list(pairs), "rs_gopls": sorted(pairs, key=pairs.get),
                "message": "Both sequences reference GoLand candidate order numbers",
            })
        return {order: candidates[index] for order, index in pairs.items()}

    def compare_candidate(self, expected, candidate, origin, failures, divergences):
        item, name = candidate.item, expected["lookupString"]
        query = self.query
        row = {"goland": expected, "rs_gopls": item}
        if candidate.replacement != tuple(expected["replacementRange"]) or (
            candidate.insert is not None and candidate.insert != tuple(expected["prefixRange"])
        ):
            failures.append({"category": "different_utf16_range", **row})

        local = query.id in LOCAL_SHADOWS
        if name == "append" and (local or query.id == "shadow-import-package-error"):
            is_variable = declared_append(query, self.source, candidate, local=local)
            is_import = item.get("kind") == 9 and candidate.path == "strings"
            if not is_variable and (local or not is_import):
                failures.append({"category": "wrong_shadow_binding", **row})
            elif local or (is_variable and expected["kind"] == "PsiDirectoryImpl"):
                divergences.append({"category": "goland_shadowing_race", **row})
            return

        if item.get("kind") not in goland_kinds(query, expected):
            failures.append({"category": "different_candidate_kind", **row})
        if bool(item.get("deprecated") or 1 in item.get("tags", ())) != expected["deprecated"]:
            failures.append({"category": "different_deprecation", **row})
        package = expected["sourcePackage"]
        if expected["kind"] == "PsiDirectoryImpl" and query.row["kind"] != "importPath":
            directory = expected["sourcePath"] or ""
            if directory.startswith("$GOROOT/src/"):
                package = directory.removeprefix("$GOROOT/src/")
        if (candidate.path and package and candidate.path != package) or (
            candidate.origin and origin and candidate.origin != origin and (
                candidate.target.get("file", "").endswith(".go") and expected["kind"] != "PsiDirectoryImpl"
            )
        ):
            failures.append({"category": "different_candidate_source", **row})
        if query.id == "selector-generic" and name in {"Value", "Get", "Set"}:
            data = item.get("data") or {}
            detail = item.get("detail") or data.get("detail") or ""
            patterns = {"Value": r"\bValue\s+string$", "Get": r"\bGet\(\)\s+string$",
                        "Set": r"\bSet\([^)]*\bstring\)$"}
            if re.search(patterns[name], detail):
                divergences.append({"category": "instantiated_generic_signature", **row})
            else:
                failures.append({"category": "missing_generic_substitution", **row})

    def compare_open_import(self, candidates, failures, divergences):
        query = self.query
        line, end = self.point
        leading = self.source.text[self.source.starts[line]:self.source.offset(self.point)]
        quote = max(leading.rfind('"'), leading.rfind("`"))
        start = len(leading[:quote + 1].encode("utf-16-le")) // 2
        prefix, delimiter = leading[quote + 1:], leading[quote]
        needed = set(OPEN_IMPORT_ANCHORS[query.id])
        exact_names = False
        if query.id == "import-workspace" and "import-workspace-closed" in self.corpus.queries:
            closed = self.corpus.queries["import-workspace-closed"]
            needed.update(c["lookupString"] for c in closed.row["candidates"] if not goland_omission(closed, c))
            exact_names = True
        names = Counter(item.name for item in candidates)
        for item in candidates:
            expected_range = line, start, line, end
            path = item.name
            if (
                item.item.get("kind") not in {9, 19} or not path.startswith(prefix) or excluded_directory(path)
                or exact_names and path not in needed
                or path in {"example.test/completion/imports_edit", "example.test/completion/cmd/app"}
                or (path.startswith("internal/") or "/internal/" in path) and not path.startswith(
                    "github.com/anthropic/chatting/mm/server/internal/"
                ) or item.replacement != expected_range or item.insert not in (None, expected_range)
                or item.new_text != path + delimiter or names[path] != 1
            ):
                failures.append({"category": "invalid_unclosed_import_candidate", "rs_gopls": item.item})
        if missing := sorted(needed - names.keys()):
            failures.append({"category": "missing_unclosed_import_candidates", "expected": missing})
        if names:
            divergences.append({"category": "completion_in_unclosed_import_literal", "goland": [],
                                "rs_gopls": [item.item for item in candidates]})

    def accept(self, client, candidates, pairs, comparison):
        acceptance = self.goland["acceptance"]
        target = acceptance["target"]
        result = comparison.rs_gopls["acceptance"] = {"target": target}
        matches = [pairs[order] for order in acceptance["matchingOrders"] if order in pairs]
        if acceptance["status"] == "missing" and not any(c.name == target["lookupString"] for c in candidates):
            result["status"] = "missing"
            return
        if len(matches) != 1:
            result["status"] = "missing" if not matches else "ambiguous"
            comparison.failures.append({"category": "acceptance_target", "target": target, "status": result["status"]})
            return
        candidate = matches[0]
        if target.get("sourcePath") and candidate.origin != self.corpus.origin(target["sourcePath"]):
            comparison.failures.append({"category": "acceptance_target_source", "target": target, "item": candidate.item})
            return
        item = candidate.item
        result["item"] = item
        if item.get("data") is not None and any(item.get(field) is None for field in RESOLVE_PROPERTIES):
            response, comparison.resolve_ms = timed_request(client, RESOLVE, item)
            if "result" not in response:
                comparison.failures.append({"category": "resolve_protocol_error", "response": response})
                return
            resolved = response["result"]
            result["resolved"] = resolved
            immutable = ("label", "kind", "sortText", "filterText", "textEdit", "insertTextFormat")
            if not isinstance(resolved, dict) or any(resolved.get(key) != item.get(key) for key in immutable):
                comparison.failures.append({"category": "resolve_changed_completion_identity", "fields": immutable})
                return
            item = resolved
        try:
            text = apply_completion(self.source, self.point, item, replace=target.get("select") == "replace")
        except (KeyError, TypeError, ValueError) as exc:
            comparison.failures.append({"category": "invalid_acceptance_edits", "message": str(exc)})
            return
        result.update(status="accepted", documentText=text)
        expected = acceptance["documentText"]
        if text == self.source.capture_text(expected):
            return
        alternative = alternate_acceptance(self.query, expected)
        if alternative and text == self.source.capture_text(alternative[1]):
            comparison.intentional_divergences.append({"category": alternative[0], "goland": acceptance,
                                                       "rs_gopls": result})
        else:
            comparison.failures.append({"category": "different_accepted_document", "goland": acceptance,
                                        "rs_gopls": result})


def completion_arguments(parser):
    parser.add_argument("server", type=executable)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=90.0)
    parser.add_argument("--json", dest="json_out", type=Path)


def validate_timeout(parser, timeout):
    if not math.isfinite(timeout) or timeout <= 0:
        parser.error("--timeout must be positive and finite")


def replay(args, corpus, environment, report):
    with completion_session(args.server, corpus.root, args.timeout, environment=environment) as (client, capabilities):
        if not capabilities.get("completionProvider"):
            raise RuntimeError("server did not advertise completionProvider")
        buffers = OpenBuffers(client, corpus)
        for query in corpus.queries.values():
            with buffers.replay(query):
                comparison = ExpectedCompletion(corpus, query).request(client)
            context = {key: query.row[key] for key in ("id", "kind", "path", "line", "character", "lineText")}
            if not comparison.failures:
                report["core_passed"] += 1
            if comparison.matched:
                report["matched"].append(context)
                continue
            details = {**context, "goland": comparison.goland, "rs_gopls": comparison.rs_gopls}
            if comparison.intentional_divergences:
                report["intentional_divergences"].append({**details, "differences": comparison.intentional_divergences})
            if comparison.failures:
                report["failures"].append({**details, "reasons": comparison.failures})


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    completion_arguments(parser)
    args = parser.parse_args(argv)
    validate_timeout(parser, args.timeout)
    report = {"matched": [], "core_passed": 0, "intentional_divergences": [], "failures": []}
    try:
        corpus = CompletionCorpus(args.workspace, args.fixture)
        environment, version = go_environment(corpus, args.timeout)
        corpus.load_active_packages(environment, args.timeout)
        report.update(workspace=str(corpus.root), fixture=str(corpus.fixture), go_version=version,
                      goland_build=corpus.data["build"], server=args.server, query_count=len(corpus.queries))
        replay(args, corpus, environment, report)
    except (KeyError, OSError, RuntimeError, TypeError, ValueError, subprocess.SubprocessError) as exc:
        report["failures"].append({"reasons": [{"category": "replay_session_or_fixture_error", "message": str(exc)}]})
    if args.json_out:
        try:
            write_report(args.json_out, report)
        except OSError as exc:
            print(f"FAIL {exc}", file=sys.stderr)
            return 1
    for failure in report["failures"]:
        reasons = ", ".join(dict.fromkeys(reason.get("message", reason["category"]) for reason in failure["reasons"]))
        print(f"FAIL {failure.get('id', 'completion replay')}: {reasons}", file=sys.stderr)
    print(f"{len(report['matched'])} matched, {report['core_passed']} core passed, "
          f"{len(report['intentional_divergences'])} with intentional differences, {len(report['failures'])} failed")
    return int(bool(report["failures"]))


if __name__ == "__main__":
    raise SystemExit(main())
