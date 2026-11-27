#!/usr/bin/env python3
"""Replay GoLand's recorded single-file editor queries over stdio LSP."""

from __future__ import annotations

import argparse
from collections import Counter
from contextlib import contextmanager
from dataclasses import asdict, dataclass, field
from hashlib import sha256
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from lsp_harness import LspClient, WORKSPACE_ENVIRONMENT, executable


FIXTURES = Path(__file__).resolve().parent / "fixtures" / "editor"
METHODS = {
    "semantic": "textDocument/semanticTokens/full",
    "highlight": "textDocument/documentHighlight",
    "inlay": "textDocument/inlayHint",
    "folding": "textDocument/foldingRange",
}
QUERY_COUNTS = {method: count for method, count in zip(METHODS.values(), (5, 69, 7, 4))}
TOKEN_TYPES = [
    "namespace", "type", "struct", "interface", "typeParameter", "parameter",
    "variable", "property", "function", "method", "label",
]
TOKEN_MODIFIERS = ["declaration", "readonly", "defaultLibrary", "modification"]


def position(value):
    point = value["line"], value["character"]
    if any(type(unit) is not int or unit < 0 for unit in point):
        raise ValueError(f"invalid UTF-16 position {value!r}")
    return point


def span(value):
    start, end = position(value["start"]), position(value["end"])
    if start > end:
        raise ValueError(f"reversed UTF-16 range {value!r}")
    return (*start, *end)


class Source:
    def __init__(self, raw):
        self.raw = raw
        self.text = raw.decode("utf-8")
        self.lines = tuple(self.text.replace("\r\n", "\n").split("\n"))
        self.utf16 = tuple(line.encode("utf-16-le") for line in self.lines)

    def column_count(self, line):
        return len(self.utf16[line]) // 2

    def check_position(self, point):
        line, column = point
        if (type(line) is not int or type(column) is not int or
                not 0 <= line < len(self.lines) or not 0 <= column <= self.column_count(line)):
            raise ValueError(f"position {point} is outside the source in UTF-16")
        if column < self.column_count(line) and 0xDC <= self.utf16[line][2 * column + 1] <= 0xDF:
            raise ValueError(f"position {point} splits a UTF-16 surrogate pair")
        return point

    def slice(self, extent):
        start_line, start_col = self.check_position(extent[:2])
        end_line, end_col = self.check_position(extent[2:])
        if extent[:2] > extent[2:]:
            raise ValueError(f"reversed source range {extent}")
        if start_line == end_line:
            return self.utf16[start_line][2 * start_col:2 * end_col].decode("utf-16-le")
        return "\n".join(line.decode("utf-16-le") for line in (
            self.utf16[start_line][2 * start_col:], *self.utf16[start_line + 1:end_line],
            self.utf16[end_line][:2 * end_col],
        ))


class Corpus:
    def __init__(self, root):
        query_bytes = (root / "editor-queries.json").read_bytes()
        self.go_mod = (root / "go.mod").read_bytes()
        query_data = json.loads(query_bytes)
        baseline = json.loads((root / "goland-editor.json").read_bytes())
        for data in (query_data, baseline):
            if data["schemaVersion"] != 1 or data["positionEncoding"] != "utf-16":
                raise ValueError("expected schema version 1 and UTF-16 fixtures")
        self.check_hash(query_bytes, baseline["querySha256"], "editor-queries.json")
        self.check_hash(self.go_mod, baseline["goModSha256"], "go.mod")
        self.sources = {}
        for name, metadata in baseline["documents"].items():
            if Path(name).name != name or not name.endswith(".go"):
                raise ValueError(f"invalid fixture document name {name!r}")
            raw = (root / name).read_bytes()
            self.check_hash(raw, metadata["sha256"], name)
            source = Source(raw)
            ending = "CRLF" if b"\r\n" in raw else "LF"
            if ending != metadata["lineEnding"] or len(source.lines) != metadata["lines"]:
                raise ValueError(f"{name}: recorded line count or line ending changed")
            self.sources[name] = source
        self.queries = query_data["queries"]
        self.cases = {case["id"]: case for case in baseline["cases"]}
        ids = {query["id"] for query in self.queries}
        if (len(ids) != len(self.queries) or len(self.cases) != len(baseline["cases"]) or
                ids != self.cases.keys() or Counter(query["method"] for query in self.queries) != QUERY_COUNTS):
            raise ValueError("query IDs or the 85-query capability inventory differ from the capture")
        for method, count in QUERY_COUNTS.items():
            if baseline["summary"][method] != {"total": count, "captured": count, "partial": 0, "blocked": 0}:
                raise ValueError(f"{method}: incomplete GoLand capture")
        for query in self.queries:
            case = self.cases[query["id"]]
            if case["status"] != "captured" or any(
                case.get(key) != query.get(key) for key in ("method", "path", "position", "range")
            ):
                raise ValueError(f"{query['id']}: GoLand capture differs from query")
            source = self.sources[query["path"]]
            if "position" in query:
                source.check_position(position(query["position"]))
            if "range" in query:
                source.slice(span(query["range"]))

    @staticmethod
    def check_hash(raw, expected, name):
        if sha256(raw).hexdigest() != expected:
            raise ValueError(f"{name}: SHA-256 differs from the captured fixture")

    def copy_sources(self, root):
        (root / "go.mod").write_bytes(self.go_mod)
        for path, source in self.sources.items():
            (root / path).write_bytes(source.raw)


@contextmanager
def editor_session(server, root, timeout, *, line_only):
    client = LspClient([server], root, timeout, environment={**os.environ, **WORKSPACE_ENVIRONMENT})
    try:
        caps = client.request(
            "initialize", processId=os.getpid(), rootUri=root.as_uri(), workspaceFolders=client.folders,
            initializationOptions={"diagnosticsTrigger": "Save"},
            capabilities={
                "general": {"positionEncodings": ["utf-16"]},
                "workspace": {"workspaceFolders": True},
                "textDocument": {
                    "semanticTokens": {
                        "requests": {"full": True},
                        "tokenTypes": TOKEN_TYPES, "tokenModifiers": TOKEN_MODIFIERS,
                        "formats": ["relative"], "overlappingTokenSupport": False,
                        "multilineTokenSupport": False,
                    },
                    "foldingRange": {"lineFoldingOnly": line_only},
                },
            },
        )["capabilities"]
        if caps.get("positionEncoding", "utf-16") != "utf-16":
            raise ValueError("server did not negotiate UTF-16 positions")
        for name in ("documentHighlightProvider", "inlayHintProvider", "foldingRangeProvider"):
            if name not in caps or caps[name] is False or caps[name] is None:
                raise ValueError(f"server did not advertise {name}")
        semantic = caps["semanticTokensProvider"]
        if (semantic.get("full") is not True or semantic.get("range", False) is not False or
                semantic["legend"] != {"tokenTypes": TOKEN_TYPES, "tokenModifiers": TOKEN_MODIFIERS}):
            raise ValueError(f"unexpected full semantic token capability {semantic!r}")
        client.notify("initialized")
        yield client, caps
        client.shutdown()
    except (OSError, KeyError, RuntimeError, TimeoutError, ValueError, subprocess.SubprocessError) as exc:
        log = client.stderr_tail()
        context = f"lineFoldingOnly={str(line_only).lower()}"
        raise RuntimeError(f"{context}: {exc}" + (f"\n{log}" if log else "")) from exc
    finally:
        client.close()


def replay(server, corpus, timeout, methods):
    replayed = {"column": {}, "line": {}}
    for line_only in (False, True):
        queries = [query for query in corpus.queries if query["method"] in methods and
                   (not line_only or query["method"] == METHODS["folding"])]
        if not queries:
            continue
        mode = "line" if line_only else "column"
        with tempfile.TemporaryDirectory(prefix="rs-gopls-editor-") as workspace:
            root = Path(workspace).resolve()
            corpus.copy_sources(root)
            with editor_session(server, root, timeout, line_only=line_only) as (client, caps):
                if not line_only:
                    replayed["capabilities"] = caps
                for path in dict.fromkeys(query["path"] for query in queries):
                    client.open_document(root / path, corpus.sources[path].text)
                for query in queries:
                    params = {key: query[key] for key in ("position", "range") if key in query}
                    params["textDocument"] = {"uri": (root / query["path"]).as_uri()}
                    try:
                        replayed[mode][query["id"]] = client.request(query["method"], **params)
                    except (RuntimeError, TimeoutError) as exc:
                        raise RuntimeError(f"{query['id']}: {exc}") from exc
    return replayed


@dataclass
class QueryReport:
    query_id: str
    path: str
    capability: str
    matched: int = 0
    differences: list = field(default_factory=list)
    errors: list = field(default_factory=list)

    def match(self, count=1):
        self.matched += count

    def difference(self, reason, *, expected=None, actual=None):
        self.differences.append({"reason": reason, "expected": expected, "actual": actual})

    def error(self, reason, *, expected=None, actual=None):
        self.errors.append({"reason": reason, "expected": expected, "actual": actual})


def compare_sets(expected, actual, report):
    expected, actual = set(expected), set(actual)
    report.match(len(expected & actual))
    for row in sorted(expected - actual, key=repr):
        report.error("missing result", expected=row)
    for row in sorted(actual - expected, key=repr):
        report.error("unexpected result", actual=row)


# GoLand's native keys cover symbol kinds, builtins and constants. It reuses
# the same key for declarations and references in many of these categories.
SEMANTIC_ATTRIBUTES = {
    "GO_PACKAGE": "namespace",
    "GO_TYPE_SPECIFICATION": "type",
    "GO_TYPE_REFERENCE": "type",
    "GO_PACKAGE_EXPORTED_STRUCT": "struct",
    "GO_PACKAGE_LOCAL_STRUCT": "struct",
    "GO_EXPORTED_STRUCT_REFERENCE": "struct",
    "GO_PACKAGE_EXPORTED_INTERFACE": "interface",
    "GO_PACKAGE_LOCAL_INTERFACE": "interface",
    "GO_EXPORTED_INTERFACE_REFERENCE": "interface",
    "GO_FUNCTION_PARAMETER": "parameter",
    "GO_METHOD_RECEIVER": "parameter",
    "GO_STRUCT_EXPORTED_MEMBER": "property",
    "GO_PACKAGE_EXPORTED_VARIABLE": "variable",
    "GO_PACKAGE_LOCAL_VARIABLE": "variable",
    "GO_LOCAL_VARIABLE": "variable",
    "GO_LOCAL_VARIABLE_CALL": "variable",
    "GO_SCOPE_VARIABLE": "variable",
    "GO_SHADOWING_VARIABLE": "variable",
    "GO_REASSIGNMENT_IN_SHORT_VAR_DECLARATION": "variable",
    "GO_LOCAL_FUNCTION": "function",
    "GO_LOCAL_FUNCTION_CALL": "function",
    "GO_EXPORTED_FUNCTION": "function",
    "GO_EXPORTED_FUNCTION_CALL": "function",
    "GO_PACKAGE_EXPORTED_CONSTANT": "variable",
    "GO_PACKAGE_LOCAL_CONSTANT": "variable",
    "GO_LOCAL_CONSTANT": "variable",
    "GO_BUILTIN_CONSTANT": "variable",
    "GO_BUILTIN_VARIABLE": "variable",
    "GO_BUILTIN_TYPE_REFERENCE": "type",
    "GO_BUILTIN_FUNCTION": "function",
    "GO_BUILTIN_FUNCTION_CALL": "function",
    "GO_LABEL": "label",
}

# Zero-based UTF-16 row and columns from the fixture source. Native
# annotator keys don't reliably distinguish a declaration from a reference,
# or reads from writes. These positions provide the LSP modifier oracle.
SEMANTIC_DECLARATIONS = {
    "semantic-editor": {
        0: (8,), 3: (1,), 5: (1,), 9: (1,), 10: (1,), 13: (4,), 15: (5,),
        17: (5,), 18: (1,), 21: (5,), 22: (1,), 23: (1,), 24: (1,), 27: (5,),
        29: (5,), 30: (1, 6, 18), 33: (5, 9), 34: (1,), 37: (5, 9, 17, 31),
        41: (6, 23, 28, 44), 46: (5,), 52: (5,), 61: (2,), 66: (5,), 67: (5,),
        68: (5,), 71: (5,), 73: (7,), 75: (7,), 76: (7,), 77: (2, 14),
        78: (2, 17), 79: (2, 8), 80: (2,), 81: (8,), 82: (8,), 87: (5, 18),
        89: (5, 12, 24, 37, 47, 56), 91: (5,), 92: (7,), 93: (5,), 94: (5, 8),
        111: (4,), 113: (2,), 119: (5, 11), 132: (1,), 139: (2, 16),
        145: (5, 11, 24), 146: (0,), 187: (5, 16, 24, 34), 201: (5,), 203: (5, 20),
    },
    "semantic-boundary": {0: (8,), 2: (6,), 4: (5, 18), 6: (5,)},
    "semantic-inlay": {
        0: (8,), 2: (5,), 4: (20, 27), 6: (5, 26), 8: (5, 21), 10: (5,),
        11: (1,), 12: (1, 8), 13: (1,), 14: (1,), 17: (5, 13, 20, 32), 18: (5,),
        19: (5, 20), 20: (5, 14, 29), 21: (5,), 22: (5, 21, 35, 44),
        23: (5, 17, 30), 25: (5, 16, 23), 33: (6,), 34: (1, 15, 27), 37: (1,),
        44: (1, 13), 46: (1, 16), 52: (5, 17), 53: (1,), 54: (1,), 60: (5, 16, 24, 43),
        71: (1,), 72: (1,), 73: (1, 8), 74: (1, 13), 77: (6,), 78: (6,), 79: (6,),
        80: (6,), 81: (6,), 82: (6,), 83: (6,),
    },
    "semantic-crlf": {
        0: (8,), 23: (4,), 26: (4,), 32: (4,), 35: (4,), 41: (1,), 42: (1,),
        46: (1,), 53: (1,), 54: (1,), 58: (1,), 63: (1,), 64: (1,), 68: (1,),
        69: (2,), 73: (5,), 74: (1, 6), 77: (5,), 78: (1,), 79: (2,),
        83: (5, 16, 29), 84: (4,), 89: (5,), 95: (1, 16),
    },
    "semantic-recovery": {
        2: (8,), 5: (4,), 7: (6,), 8: (6,), 10: (5, 22), 12: (5,), 16: (5,), 18: (5,),
    },
}
SEMANTIC_WRITES = {
    "semantic-editor": {
        13: (29,), 42: (1,), 50: (11,), 51: (12, 31, 42), 53: (18,), 95: (1,),
        96: (1,), 97: (1,), 98: (1,), 99: (1,), 100: (1,), 101: (2,), 102: (2,),
        104: (5,), 106: (12,), 108: (5, 8), 111: (1,), 188: (1,),
    },
    "semantic-inlay": {53: (31,), 54: (25,), 55: (46,), 57: (16, 26, 39)},
}

# Intentional projections are scoped to individual annotated names. The
# tuple includes the spelling so a different token at the same position
# does not silently inherit an exception.
SEMANTIC_OMISSIONS = {
    "semantic-editor": {
        (47, 11, "Second", "GO_PACKAGE_EXPORTED_CONSTANT"): "external-package-member",
        (48, 13, "ToUpper", "GO_EXPORTED_FUNCTION_CALL"): "external-package-member",
        (49, 5, "Pi", "GO_PACKAGE_EXPORTED_CONSTANT"): "dot-import-member",
        (57, 5, "boundaryValue", "GO_PACKAGE_LOCAL_CONSTANT"): "same-package-other-file",
        (58, 5, "boundaryCall", "GO_LOCAL_FUNCTION_CALL"): "same-package-other-file",
    },
    "semantic-boundary": {
        (6, 33, "PackageVariable", "GO_PACKAGE_EXPORTED_VARIABLE"): "same-package-other-file",
        (6, 49, "F", "GO_STRUCT_EXPORTED_MEMBER"): "same-package-other-file",
    },
    "semantic-inlay": {
        (19, 13, "_", "GO_FUNCTION_PARAMETER"): "blank-identifier",
        (48, 1, "boundaryCall", "GO_LOCAL_FUNCTION_CALL"): "same-package-other-file",
        (60, 36, "_", "GO_FUNCTION_PARAMETER"): "blank-identifier",
        (78, 10, "0x10", "GO_NUMBER"): "non-symbol-annotation",
    },
    "semantic-crlf": {
        (124, 5, "Println", "GO_EXPORTED_FUNCTION_CALL"): "external-package-member",
        (124, 29, "Itoa", "GO_EXPORTED_FUNCTION_CALL"): "external-package-member",
    },
    "semantic-recovery": {(0, 2, "go:build", "GO_COMMENT_KEYWORD"): "non-symbol-annotation"},
}
SEMANTIC_REFINEMENTS = {
    "semantic-editor": {
        (22, 1, "Embedded"): ("property", "embedded-field-declaration"),
        (30, 1, "Read"): ("method", "method-token-kind"),
        (41, 23, "Read"): ("method", "method-token-kind"),
        (55, 10, "Read"): ("method", "method-token-kind"),
        (56, 14, "Read"): ("method", "method-token-kind"),
    },
    "semantic-inlay": {
        (4, 20, "method"): ("method", "method-token-kind"),
        (11, 1, "HintEmbedded"): ("property", "embedded-field-declaration"),
        (36, 10, "method"): ("method", "method-token-kind"),
        (37, 25, "method"): ("method", "method-token-kind"),
        (39, 14, "method"): ("method", "method-token-kind"),
    },
    "semantic-crlf": {(74, 1, "Call"): ("method", "method-token-kind")},
}
TYPE_PARAMETER_TOKENS = {
    "semantic-editor": ((33, 9), (34, 9), (37, 9), (37, 27), (37, 37)),
}


def semantic_modifiers(case_id, start, attribute):
    line, column = start
    declared = column in SEMANTIC_DECLARATIONS[case_id].get(line, ())
    modified = column in SEMANTIC_WRITES.get(case_id, {}).get(line, ())
    return tuple(
        name for name, enabled in zip(
            TOKEN_MODIFIERS,
            (declared, attribute.endswith("_CONSTANT"), attribute.startswith("GO_BUILTIN_"), modified),
        )
        if enabled
    )


def decode_semantic_tokens(actual, source, legend):
    types, modifier_names = legend["tokenTypes"], legend["tokenModifiers"]
    data = actual.get("data") if isinstance(actual, dict) else None
    if not isinstance(data, list) or len(data) % 5:
        raise ValueError("semantic token data must contain groups of five integers")
    tokens = {}
    line = start = end = 0
    for offset in range(0, len(data), 5):
        group = data[offset:offset + 5]
        if any(type(n) is not int or n < 0 or n >= 2**31 for n in group):
            raise ValueError(f"invalid semantic token data at index {offset}: {group!r}")
        delta_line, delta_start, length, type_index, mask = group
        if not length or type_index >= len(types) or mask >= 1 << len(modifier_names):
            raise ValueError(f"invalid semantic token length, type or modifiers at index {offset}: {group!r}")
        if delta_line == 0 and start + delta_start < end:
            raise ValueError(f"semantic token at index {offset} is duplicated, overlapping or out of order")
        line += delta_line
        start = delta_start if delta_line else start + delta_start
        end = start + length
        token_span = (line, start, line, end)
        modifiers = tuple(
            name for index, name in enumerate(modifier_names) if mask & (1 << index)
        )
        tokens[token_span] = (source.slice(token_span), types[type_index], modifiers)
    return tokens


def compare_semantic(query, case, actual, source, report, legend):
    case_id = query["id"]
    observed = decode_semantic_tokens(actual, source, legend)
    expected = {}
    seen = set()
    for row in case["items"]:
        token_span = span(row["range"])
        text, attribute = row["text"], row["attribute"]
        if token_span in seen or not text or source.slice(token_span) != text:
            raise ValueError(f"invalid or duplicated GoLand annotation {row!r}")
        seen.add(token_span)

        identity = (*token_span[:2], text)
        omission = SEMANTIC_OMISSIONS.get(case_id, {}).get((*identity, attribute))
        native_token = (token_span, text, attribute)
        if omission:
            expected[token_span] = (None, omission, native_token)
            continue
        kind = SEMANTIC_ATTRIBUTES.get(attribute)
        if kind is None:
            report.error("unmapped GoLand annotation", expected=row)
            continue
        refinement = SEMANTIC_REFINEMENTS.get(case_id, {}).get(identity)
        if refinement:
            refined_kind, difference = refinement
            native_kind = "function" if refined_kind == "method" else "struct"
            if kind != native_kind:
                report.error("unexpected GoLand category for refined semantic token", expected=row)
                continue
            kind = refined_kind
        else:
            difference = None
        token = (text, kind, semantic_modifiers(case_id, token_span[:2], attribute))
        expected[token_span] = (token, difference, native_token)

    for line, column in TYPE_PARAMETER_TOKENS.get(case_id, ()):
        token_span = (line, column, line, column + 1)
        if token_span in seen or source.slice(token_span) != "T":
            raise ValueError(f"unexpected GoLand annotation or source for type parameter {token_span}")
        token = ("T", "typeParameter", semantic_modifiers(case_id, (line, column), ""))
        expected[token_span] = (token, "type-parameter-token", None)

    for token_span, (wanted, difference, native_token) in expected.items():
        found = observed.pop(token_span, None)
        actual_token = None if found is None else (token_span, found)
        if found == wanted:
            if difference:
                report.difference(difference, expected=native_token, actual=actual_token)
            else:
                report.match()
        elif wanted is None:
            report.error("extra semantic token for omitted GoLand annotation", actual=actual_token)
        else:
            report.error(
                "missing semantic token" if found is None else "semantic token category or modifiers",
                expected=(token_span, wanted),
                actual=actual_token,
            )
    for token_span, token in observed.items():
        report.error("extra semantic token", actual=(token_span, token))


HIGHLIGHT_KINDS = {"TEXT": 1, "READ": 2, "WRITE": 3, "ReadWrite": 3}
HIGHLIGHT_TEXT_REFERENCES = {
    "import-alias": "clock",
    "import-implicit": "strings",
    "interface-method": "Read",
    "non-bmp-prefix": "strconv",
    "loop-label-declaration": "Outer",
    "loop-labelled-break-target": "Outer",
    "return-forwarded-call": "slotSource",
    "return-anonymous-forwarded-call": "slotSource",
    "return-anonymous-first-type": "int",
    "return-anonymous-second-type": "string",
}
HIGHLIGHT_BOUNDARIES = {
    "import-dot-use": ((49, 5, 49, 7), "Pi", "dot import member"),
    "external-selector": ((47, 11, 47, 17), "Second", "external package member"),
    "cross-file-constant": ((57, 5, 57, 18), "boundaryValue", "same-package cross-file constant"),
    "cross-file-function": ((58, 5, 58, 17), "boundaryCall", "same-package cross-file function"),
}


def highlight_kind(query_id, item):
    access = HIGHLIGHT_KINDS[item["access"]]
    text = item["text"]
    if item["customHandlerRange"]:
        # The named result declaration also has a regular read occurrence.
        if query_id == "return-named-declaration" and text == "left":
            return access
        return HIGHLIGHT_KINDS["TEXT"]
    if HIGHLIGHT_TEXT_REFERENCES.get(query_id) == text:
        return HIGHLIGHT_KINDS["TEXT"]
    return access


def compare_highlights(query, case, actual, source, report):
    if actual is None:
        actual = []
    if not isinstance(actual, list):
        raise ValueError("documentHighlight result must be an array or null")

    observed = {}
    for item in actual:
        coordinates = span(item["range"])
        text = source.slice(coordinates)
        kind = item.get("kind")
        if not text or type(kind) is not int or kind not in (1, 2, 3):
            report.error("invalid document highlight", actual=item)
            continue
        if coordinates in observed:
            report.error("duplicate document highlight range", actual=item)
            continue
        observed[coordinates] = kind

    expected = {}
    for item in case["items"]:
        coordinates = span(item["range"])
        text = item["text"]
        if not text or source.slice(coordinates) != text or coordinates in expected:
            raise ValueError(f"invalid GoLand document highlight range {coordinates}")
        expected[coordinates] = (text, highlight_kind(query["id"], item))

    boundary = HIGHLIGHT_BOUNDARIES.get(query["id"])
    if boundary:
        coordinates, text, reason = boundary
        if coordinates not in expected or expected[coordinates][0] != text:
            raise ValueError(f"missing recorded {reason} range {coordinates}")
        _, kind = expected.pop(coordinates)
        if coordinates not in observed:
            report.difference(
                f"single-file boundary: {reason}",
                expected=(coordinates, text, kind),
            )

    for coordinates, (text, kind) in sorted(expected.items()):
        observed_kind = observed.pop(coordinates, None)
        if observed_kind is None:
            report.error("missing document highlight", expected=(coordinates, text, kind))
        elif observed_kind != kind:
            report.error(
                "document highlight kind mismatch",
                expected=(coordinates, text, kind),
                actual=(coordinates, text, observed_kind),
            )
        else:
            report.match()
    for coordinates, kind in sorted(observed.items()):
        report.error(
            "extra document highlight",
            actual=(coordinates, source.slice(coordinates), kind),
        )


def inlay_items(items, source, extent):
    if not isinstance(items, list):
        raise ValueError(f"expected a list of inlay hints, got {items!r}")
    rows = set()
    for item in items:
        point = source.check_position(position(item["position"]))
        if not extent[:2] <= point < extent[2:]:
            raise ValueError(f"inlay hint at {point} is outside the requested range {extent}")
        label = item["label"]
        kind = item.get("kind")
        padding_left = item.get("paddingLeft", False)
        padding_right = item.get("paddingRight", False)
        if (not isinstance(label, str) or not label or
                ("kind" in item and (type(kind) is not int or kind not in (1, 2))) or
                type(padding_left) is not bool or type(padding_right) is not bool):
            raise ValueError(f"invalid inlay hint {item!r}")
        row = point, label, kind, padding_left, padding_right
        if row in rows:
            raise ValueError(f"duplicate inlay hint {row!r}")
        rows.add(row)
    return rows


def compare_inlay(query, case, actual, source, report):
    extent = span(query["range"])
    expected = inlay_items(case["items"], source, extent)
    actual = inlay_items([] if actual is None else actual, source, extent)
    if query["id"] == "inlay-calls":
        boundary = ((48, 14), "amount:", 2, False, True)
        expected.remove(boundary)
        report.difference("single-file boundary: boundaryCall signature", expected=boundary)
    if query["id"] == "inlay-constants":
        goland = ((80, 16), "= 4", None, True, False)
        go = ((80, 16), "= 10", None, True, False)
        expected.remove(goland)
        if go in actual:
            actual.remove(go)
            report.difference("GoLand string len counts UTF-16 units; Go counts UTF-8 bytes",
                              expected=goland, actual=go)
        else:
            expected.add(go)
    compare_sets(expected, actual, report)


FOLD_KINDS = {None, "comment", "imports", "region"}


def fold_crosses(first, second):
    return (first[:2] < second[:2] < first[2:] < second[2:] or
            second[:2] < first[:2] < second[2:] < first[2:])


def fold_columns(items, source, report):
    columns = []
    groups = {}
    for item in items:
        extent = span(item["range"])
        kind = item.get("kind")
        if not source.slice(extent) or kind not in FOLD_KINDS:
            raise ValueError(f"invalid GoLand folding descriptor {item!r}")
        if "group" in item:
            groups.setdefault(item["group"], []).append((extent, item))
        else:
            columns.append((extent, kind))

    for fragments in groups.values():
        if len(fragments) != 2:
            raise ValueError(f"expected two linked GoLand folding fragments, got {fragments!r}")
        (opening, first), (closing, second) = sorted(fragments, key=lambda row: row[0])
        if (first.get("kind") is not None or second.get("kind") is not None or
                (first["placeholder"], second["placeholder"]) not in {("{ ", " }"), ("{", "}")} or
                first["elementType"] != second["elementType"] or opening[2:] > closing[:2] or
                not source.slice(opening).startswith("{") or not source.slice(closing).endswith("}")):
            raise ValueError(f"invalid linked GoLand folding fragments {fragments!r}")
        row = ((*opening[:2], *closing[2:]), None)
        columns.append(row)
        if report is not None:
            report.difference("protocol projection: two linked block fragments become one range",
                              expected=(opening, closing), actual=row)

    regions = [extent for extent, kind in columns if kind == "region"]
    projected = []
    for extent, kind in columns:
        conflict = next((region for region in regions
                         if region == extent or fold_crosses(region, extent)), None)
        if kind == "comment" and conflict is not None:
            if report is not None:
                report.difference("protocol projection: region takes precedence over crossing or equal comment",
                                  expected=(extent, kind), actual=(conflict, "region"))
        else:
            projected.append((extent, kind))
    return projected


def fold_lines(columns, source, report):
    lines = []
    for extent, kind in columns:
        start_line, _, end_line, end_col = extent
        if end_col == 0:
            end_line -= 1
        delimiter = source.lines[end_line].strip() in {"}", ")"}
        if delimiter:
            end_line -= 1
        if start_line >= end_line:
            report.difference("line-only projection: omit a range covering fewer than two lines",
                              expected=(extent, kind))
            continue
        row = ((start_line, 0, end_line, 0), kind)
        lines.append(row)
        if delimiter:
            report.difference("line-only projection: retain a delimiter-only closing line",
                              expected=(extent, kind), actual=row)
        elif end_col == 0:
            report.difference("line-only projection: stop before an exclusive ending line",
                              expected=(extent, kind), actual=row)
    return lines


def validate_folds(rows, origin):
    ranges = sorted(extent for extent, _ in rows)
    for index, first in enumerate(ranges):
        for second in ranges[index + 1:]:
            if second[:2] >= first[2:]:
                break
            if first == second or fold_crosses(first, second):
                raise ValueError(f"{origin} has duplicate or crossing folds {first} and {second}")


def compare_folding(query, case, actual, source, report, *, line_only):
    expected = fold_columns(case["items"], source, None if line_only else report)
    if line_only:
        expected = fold_lines(expected, source, report)
    validate_folds(expected, "projected GoLand baseline")
    if not isinstance(actual, list):
        raise ValueError(f"expected a folding range list, got {actual!r}")
    rows = []
    fields = {"startLine", "endLine", "kind"}
    if not line_only:
        fields.update(("startCharacter", "endCharacter"))
    for item in actual:
        if not isinstance(item, dict):
            raise ValueError(f"invalid folding response item {item!r}")
        kind = item.get("kind")
        if set(item) - fields or kind not in FOLD_KINDS or ("kind" in item and kind is None):
            raise ValueError(f"invalid folding response for {query['id']}: {item!r}")
        start = source.check_position((item["startLine"], 0 if line_only else item["startCharacter"]))
        end = source.check_position((item["endLine"], 0 if line_only else item["endCharacter"]))
        if start >= end:
            raise ValueError(f"degenerate folding range {item!r}")
        rows.append(((*start, *end), kind))
    validate_folds(rows, "LSP response")
    compare_sets(expected, rows, report)


def compare(corpus, replayed, methods):
    comparators = {
        METHODS["semantic"]: compare_semantic,
        METHODS["highlight"]: compare_highlights,
        METHODS["inlay"]: compare_inlay,
        METHODS["folding"]: compare_folding,
    }
    names = {method: name for name, method in METHODS.items()}
    legend = replayed["capabilities"]["semanticTokensProvider"]["legend"]
    for query in corpus.queries:
        query_id = query["id"]
        method = query["method"]
        if method not in methods:
            continue
        case = corpus.cases[query_id]
        source = corpus.sources[query["path"]]
        modes = ("column", "line") if method == METHODS["folding"] else ("column",)
        for mode in modes:
            capability = f"folding-{mode}" if method == METHODS["folding"] else names[method]
            report = QueryReport(query_id, query["path"], capability)
            kwargs = {}
            if method == METHODS["semantic"]:
                kwargs["legend"] = legend
            elif method == METHODS["folding"]:
                kwargs["line_only"] = mode == "line"
            try:
                if query_id not in replayed[mode]:
                    report.error("missing LSP response")
                else:
                    comparators[method](query, case, replayed[mode][query_id], source, report, **kwargs)
            except (KeyError, TypeError, ValueError) as exc:
                report.error(f"invalid baseline or LSP result: {exc}")
            yield report


def print_report(reports, *, verbose):
    errors = sum(len(report.errors) for report in reports)
    passed = {report.query_id for report in reports} - {report.query_id for report in reports if report.errors}
    query_count = len({report.query_id for report in reports})
    print(f"GoLand editor replay: {len(passed)}/{query_count} queries passed "
          f"({len(reports)} LSP requests), {errors} unexpected differences")
    for capability in ("semantic", "highlight", "inlay", "folding-column", "folding-line"):
        group = [report for report in reports if report.capability == capability]
        if not group:
            continue
        matched = sum(report.matched for report in group)
        group_passed = sum(not report.errors for report in group)
        differences = Counter(row["reason"] for report in group for row in report.differences)
        print(f"  {capability}: {group_passed}/{len(group)} passed, {matched} matched, "
              f"{differences.total()} agreed differences")
        for reason, count in sorted(differences.items()):
            print(f"    {count:3}  {reason}")
        for report in group:
            if verbose or report.errors:
                print(f"    {report.query_id} ({report.path}): {report.matched} matched")
            for label, rows in (("DIFFERENCE", report.differences if verbose else []),
                                ("ERROR", report.errors)):
                for row in rows:
                    details = " ".join(f"{key}={json.dumps(row[key])}"
                                       for key in ("expected", "actual") if row[key] is not None)
                    print(f"      {label} {row['reason']}" + (f" {details}" if details else ""))
    return errors == 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=executable, metavar="rs-gopls.exe")
    parser.add_argument("--capability", choices=METHODS, action="append", help="limit replay to a capability")
    parser.add_argument("--timeout", type=float, default=30.0, help="seconds per LSP request")
    parser.add_argument("--verbose", action="store_true", help="show each query and agreed difference")
    parser.add_argument("--report", type=Path, help="write per-query results as JSON")
    args = parser.parse_args(argv)
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be a finite positive number")
    methods = {METHODS[name] for name in args.capability or METHODS}
    try:
        corpus = Corpus(FIXTURES)
        results = list(compare(corpus, replay(args.server, corpus, args.timeout, methods), methods))
        passed = print_report(results, verbose=args.verbose)
        if args.report:
            args.report.write_text(json.dumps([asdict(report) for report in results],
                                              indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        return 0 if passed else 1
    except (OSError, KeyError, TypeError, RuntimeError, ValueError) as exc:
        print(f"GoLand editor replay failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
