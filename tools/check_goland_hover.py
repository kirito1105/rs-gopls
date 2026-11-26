#!/usr/bin/env python3
"""Check Go hover signatures, documentation, and token ranges over stdio LSP."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import math
from pathlib import Path
import re
import subprocess
import sys

from check_goland_definitions import fixture_path, utf16_offset
from compare_definitions import LspClient, executable


DEFAULT_FIXTURE = Path(__file__).resolve().parent / "fixtures" / "localchat-goland-hover-signature.json"
MARKDOWN = re.compile(r"\A(?P<fence>`{3,})go\n(?P<code>.+?)\n(?P=fence)(?:\n\n(?P<doc>.+))?\Z", re.S)
DIRECTIVE = re.compile(
    r"(?m)^\s*(?://\s*|/\*\s*|\*\s*)?(?:go:[\w-]+|\+build\b|line\s+\S+:\d+"
    r"|extern\b|export\b|noinspection\b|goland:)"
)


@dataclass(frozen=True)
class Expected:
    signature: tuple[str, ...]
    documentation: tuple[str, ...] = ()


# GoLand's capture has no body for these three resolved targets. The Hover
# contract requires package documentation and declarations at these positions.
RESOLVED_WITHOUT_GOLAND_BODY = {
    "hover-pkg-qualifier",
    "syn-hover-import-pkg-doc",
    "syn-hover-const-group-inline",
}

EXPECTATIONS = {
    "hover-interface-decl": Expected(
        ("type FileBackend interface {", "TestConnection() error", "ReadFile(path string) ([]byte, error)",
         "WriteFile(data io.Reader, path string) (int64, error)"),
        ("FileBackend defines the interface for file storage operations.",),
    ),
    "hover-func-decl-doc": Expected(
        ("func NewFileBackend(settings config.FileStoreSettings) (FileBackend, error)",),
        ("NewFileBackend creates a FileBackend based on settings.",),
    ),
    "hover-iface-method-decl": Expected(
        ("func (UserStore) GetAll(offset int, limit int, includeBots bool) ([]*model.User, error)",),
    ),
    "hover-iface-with-comment": Expected(
        ("type FavoriteStore interface {", "Toggle(userId, postId string) (bool, error)",
         "GetForUser(userId string, offset, limit int) ([]*model.Favorite, error)"),
        ('FavoriteStore persists per-user saved posts ("收藏 / 稍后读").',),
    ),
    "hover-pkg-var": Expected(
        ('var ErrReviewItemsNotFound error = errors.New("one or more review items were not found")',),
    ),
    "hover-const": Expected(("const UserPasswordMaxLength int = 72",)),
    "hover-struct-decl": Expected(
        ("type User struct {", 'Id string `json:"id"`', 'Username string `json:"username"`'),
    ),
    "hover-struct-field-tag": Expected(('field Username string `json:"username"` of User type',)),
    "hover-field-use": Expected(('field Username string `json:"username"` of User type',)),
    "hover-method-decl": Expected(("func (u *User) PreSave() error",)),
    "hover-local-inferred": Expected(("var now int64 = GetMillis()",)),
    "hover-func-type-decl": Expected(
        ("type HandlerFunc func(c *Context, w http.ResponseWriter, r *http.Request)",),
    ),
    "hover-field-inline-comment": Expected(("field APIRoot *mux.Router of Routes type",), ("/api/v1",)),
    "hover-receiver-param": Expected(("var user *model.User",)),
    "hover-cross-pkg-method-call": Expected(
        ("func (UserStore) GetAll(offset int, limit int, includeBots bool) ([]*model.User, error)",),
    ),
    "hover-pkg-qualifier": Expected(("package model",)),
    # The GoLand capture returned parent package net. Use the actual import.
    "hover-import-path": Expected(
        ('import "net/http"',),
        ("Package http provides HTTP client and server implementations.",),
    ),
    "hover-stdlib-func": Expected(
        ("func TrimSpace(s string) string",),
        ("TrimSpace returns a slice", "of the string s,", "leading and trailing white space removed",
         "defined by Unicode."),
    ),
    "hover-builtin-append": Expected(
        ("func append(slice []Type, elems ...Type) []Type",),
        ("The append built-in function appends elements to the end of a slice.",),
    ),
    # The embedded name is both a type use and a field declaration. Hover
    # presents the field at the embedding site, including its Go type.
    "hover-embedded-field-decl": Expected(("field SqlStore *SqlStore of SqlUserStore type",)),
    "hover-empty-space": None,
    "syn-hover-import-pkg-doc": Expected(
        ('import "example.com/hoversig/helper"',),
        ("Package helper provides utilities for hover and signature help baselines.",),
    ),
    "syn-hover-const-group-inline": Expected(
        ("const StatusOK int = 200",),
        ("Status codes shared across the helper package.", "request succeeded"),
    ),
    "syn-hover-const-group-fallback": Expected(
        ("const StatusError int = 500",), ("Status codes shared across the helper package.",),
    ),
    "syn-hover-pkg-var-doc": Expected(
        ('var DefaultName string = "guest"',), ("DefaultName is the fallback display name.",),
    ),
    "syn-hover-promoted-field-tag": Expected(
        ('field ID int64 `json:"id"` of Base type',), ("ID uniquely identifies the record.",),
    ),
    "syn-hover-field-tag-inline": Expected(
        ('field Name string `json:"name,omitempty"` of Item type',), ("display label",),
    ),
    "syn-hover-type-alias": Expected(("type IntBox = Box[int]",), ("IntBox aliases a specialized Box.",)),
    "syn-hover-generic-func-directive": Expected(
        ("func Map[T any, U any](items []T, fn func(T) U) []U",),
        ("Map transforms each element using fn.",),
    ),
    "syn-hover-generic-method": Expected(("func (b *Box[T]) Set(v T)",), ("Set replaces the stored value.",)),
    "syn-hover-iface-method": Expected(
        ("func (Formatter) Format(prefix string, args ...any) string",), ("Format renders args using prefix.",),
    ),
    "syn-hover-func-type": Expected(("type UnaryOp func(x int) int",), ("UnaryOp transforms a single integer.",)),
    "syn-hover-local-var-inferred": Expected(('var joined string = helper.Join("-", "a", "b", "c")',)),
    "syn-hover-func-param": Expected(("var cb func(count int, tag string) bool",)),
    "syn-hover-builtin-len": Expected(
        ("func len(v Type) int",), ("The len built-in function returns the length of v",),
    ),
}


@dataclass(frozen=True)
class Case:
    id: str
    path: Path
    position: dict[str, int]
    token_range: dict[str, dict[str, int]]
    expected: Expected | None


def offset(text, point):
    if not isinstance(point, dict) or point.keys() != {"line", "character"}:
        raise ValueError(f"invalid position {point!r}")
    if any(type(value) is not int or value < 0 for value in point.values()):
        raise ValueError(f"invalid position {point!r}")
    return utf16_offset(text, point["line"], point["character"])


def load_cases(fixture, workspace):
    corpus = json.loads(fixture.read_text(encoding="utf-8-sig"))
    cases = {}
    sources = {}
    for row in corpus["cases"]:
        if row["kind"] != "hover":
            continue
        case_id = row["id"]
        try:
            if not case_id or case_id in cases:
                raise ValueError("empty or duplicate case ID")
            path = fixture_path(row["path"], workspace)
            if path not in sources:
                sources[path] = path.read_text(encoding="utf-8")
            text = sources[path]
            position = {"line": row["line"], "character": row["character"]}
            token_range = row["leafRange"]
            start = offset(text, token_range["start"])
            end = offset(text, token_range["end"])
            if not start <= offset(text, position) < end or text[start:end] != row["leafText"]:
                raise ValueError(f"captured range at {path} no longer selects {row['leafText']!r}")
            expected = EXPECTATIONS[case_id]
            has_hover = row["hoverHtml"] is not None or case_id in RESOLVED_WITHOUT_GOLAND_BODY
            if has_hover != (expected is not None):
                raise ValueError("presence expectation disagrees with capture")
            cases[case_id] = Case(case_id, path, position, token_range, expected)
        except (KeyError, OSError, TypeError, ValueError) as exc:
            raise ValueError(f"{fixture} case {case_id!r}: {exc}") from exc
    if not cases:
        raise ValueError(f"{fixture}: no hover cases")
    return cases.values(), sources


def normalized(text):
    return " ".join(text.split())


def check_hover(case, result, text):
    expected = case.expected
    if expected is None:
        if result is not None:
            raise ValueError(f"expected null, received {result!r}")
        return
    if not isinstance(result, dict):
        raise ValueError(f"expected a Hover object, received {result!r}")
    span = result.get("range")
    if not isinstance(span, dict) or span.keys() != {"start", "end"}:
        raise ValueError(f"invalid hover range {span!r}")
    start, end = (offset(text, span[key]) for key in ("start", "end"))
    if span != case.token_range:
        raise ValueError(f"range expected {case.token_range}, received {span} selecting {text[start:end]!r}")
    contents = result.get("contents")
    if not isinstance(contents, dict) or contents.get("kind") != "markdown":
        raise ValueError(f"expected Markdown contents, received {contents!r}")
    value = contents.get("value")
    match = MARKDOWN.fullmatch(value) if isinstance(value, str) else None
    if match is None:
        raise ValueError(f"expected a fenced Go signature with optional documentation, received {value!r}")
    if DIRECTIVE.search(value) or re.search(r"\bgo:(?:noinline|build)\b", value):
        raise ValueError("compiler or GoLand directive leaked into hover")
    signature = normalized(match["code"])
    if not signature.startswith(expected.signature[0]):
        raise ValueError(f"expected signature starting with {expected.signature[0]!r}, received {signature!r}")
    for fragment in expected.signature[1:]:
        if fragment not in signature:
            raise ValueError(f"signature is missing {fragment!r}")
    documentation = normalized(match["doc"] or "")
    for fragment in expected.documentation:
        if fragment not in documentation:
            raise ValueError(f"documentation is missing {fragment!r}")


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
            capabilities = client.initialize()
            if capabilities.get("hoverProvider") is not True:
                raise RuntimeError("server did not advertise hoverProvider: true")
            for path, text in sources.items():
                client.open_document(path, text)
            failures = 0
            for case in cases:
                try:
                    result = client.request(
                        "textDocument/hover",
                        textDocument={"uri": case.path.as_uri()},
                        position=case.position,
                    )
                    check_hover(case, result, sources[case.path])
                except (OSError, RuntimeError, ValueError) as exc:
                    print(f"{case.id}: {exc}", file=sys.stderr)
                    failures += 1
                else:
                    print(f"{case.id}: {'expected null' if case.expected is None else 'matched'}")
            client.shutdown()
        finally:
            client.close()
    except (KeyError, OSError, RuntimeError, ValueError, subprocess.SubprocessError) as exc:
        print(f"GoLand hover check failed: {exc}", file=sys.stderr)
        return 1

    print(f"{len(cases) - failures}/{len(cases)} hover cases passed")
    return int(failures != 0)


if __name__ == "__main__":
    raise SystemExit(main())
