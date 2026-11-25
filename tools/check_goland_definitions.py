#!/usr/bin/env python3
"""Compare an LSP server's Go definitions with a captured GoLand corpus."""

from __future__ import annotations

import argparse
from collections import Counter
import csv
from dataclasses import dataclass
import json
import math
from pathlib import Path, PurePosixPath
import subprocess
import sys
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from check_definitions import Site


FIXTURES = {
    "definition": "localchat-goland.tsv",
    "typeDefinition": "localchat-goland-types.tsv",
}
COLUMNS = tuple(
    "id query_path query_line query_character query_token target_kind target_path "
    "target_line target_character target_end_line target_end_character target_token".split()
)
CACHE_PREFIX = "$GOMODCACHE/"
SDK_PREFIX = "$GOROOT/"
ERROR_STATUSES = {"missing", "wrong", "unexpected", "request-error"}


@dataclass(frozen=True)
class Case:
    id: str
    query: Site
    position: dict[str, int]
    target_kind: str
    target_path: str
    target: Site | None


def navigation_arguments(parser):
    parser.add_argument("--method", choices=FIXTURES, default="definition")
    parser.add_argument("--fixture", type=Path, help="defaults to the GoLand corpus for the selected method")


def navigation_fixture(method, fixture):
    return fixture if fixture is not None else Path(__file__).resolve().parent / "fixtures" / FIXTURES[method]


def utf16_offset(text, line, character):
    if line < 0 or character < 0:
        raise ValueError(f"negative position {line}:{character}")
    start = 0
    for _ in range(line):
        newline = text.find("\n", start)
        if newline == -1:
            raise ValueError(f"line {line} is outside the source")
        start = newline + 1
    end = text.find("\n", start)
    if end == -1:
        end = len(text)
    column = 0
    for offset in range(start, end):
        if column == character:
            return offset
        column += 2 if ord(text[offset]) > 0xFFFF else 1
    if column == character:
        return end
    raise ValueError(f"invalid UTF-16 character {character} on line {line}")


def go_path(workspace, key):
    value = subprocess.check_output(
        ["go", "env", key], cwd=workspace, encoding="utf-8"
    ).strip()
    if not value:
        raise ValueError(f"go env {key} returned an empty path")
    return Path(value).resolve()


def fixture_path(path, root):
    relative = PurePosixPath(path)
    if not relative.parts or relative.is_absolute() or any(
        part == ".." or ":" in part or "\\" in part for part in relative.parts
    ):
        raise ValueError(f"expected a relative POSIX path, received {path!r}")
    result = root.joinpath(*relative.parts).resolve()
    if not result.is_relative_to(root):
        raise ValueError(f"path {path!r} escapes {root}")
    return result


def query_site(path, text, position, token):
    from check_definitions import Site

    caret = utf16_offset(text, position["line"], position["character"])
    starts = [
        start
        for start in range(max(0, caret - len(token) + 1), caret + 1)
        if text.startswith(token, start)
    ]
    if len(starts) != 1:
        raise ValueError(f"caret at {path}:{position} does not identify {token!r} uniquely")
    start = starts[0]
    return Site(path, text, start, start + len(token))


def load_cases(fixture: Path, workspace: Path, gomodcache: Path | None = None) -> list[Case]:
    """Load the corpus, checking every pinned token and UTF-16 coordinate."""
    from check_definitions import Site

    workspace = workspace.resolve()
    if gomodcache is not None:
        gomodcache = gomodcache.resolve()
    goroot = None
    cases = []
    ids = set()
    sources = {}

    def source(path):
        if path not in sources:
            sources[path] = path.read_text(encoding="utf-8")
        return sources[path]

    with fixture.open(encoding="utf-8-sig", newline="") as stream:
        rows = csv.DictReader(stream, delimiter="\t", quoting=csv.QUOTE_NONE)
        if tuple(rows.fieldnames or ()) != COLUMNS:
            raise ValueError(f"{fixture}: unexpected corpus columns")
        for row in rows:
            try:
                if None in row or any(value is None for value in row.values()):
                    raise ValueError("unexpected field count")
                case_id = row["id"]
                if not case_id or case_id in ids:
                    raise ValueError(f"empty or duplicate case id {case_id!r}")

                position = {
                    "line": int(row["query_line"]),
                    "character": int(row["query_character"]),
                }
                path = fixture_path(row["query_path"], workspace)
                query = query_site(path, source(path), position, row["query_token"])
                kind = row["target_kind"]
                target_path = row["target_path"]
                target = None
                if kind == "file":
                    if target_path.startswith(CACHE_PREFIX):
                        if gomodcache is None:
                            gomodcache = go_path(workspace, "GOMODCACHE")
                        path = fixture_path(target_path.removeprefix(CACHE_PREFIX), gomodcache)
                    elif target_path.startswith(SDK_PREFIX):
                        if goroot is None:
                            goroot = go_path(workspace, "GOROOT")
                        path = fixture_path(target_path.removeprefix(SDK_PREFIX), goroot)
                    else:
                        path = fixture_path(target_path, workspace)
                    text = source(path)
                    start = utf16_offset(text, int(row["target_line"]), int(row["target_character"]))
                    end = utf16_offset(text, int(row["target_end_line"]), int(row["target_end_character"]))
                    if start >= end or text[start:end] != row["target_token"]:
                        raise ValueError(f"{path} target range does not select {row['target_token']!r}")
                    target = Site(path, text, start, end)
                elif kind == "package":
                    if not target_path:
                        raise ValueError("package target requires a path")
                elif kind != "empty":
                    raise ValueError(f"unknown target kind {kind!r}")
                cases.append(Case(case_id, query, position, kind, target_path, target))
                ids.add(case_id)
            except (OSError, ValueError, subprocess.SubprocessError) as exc:
                raise ValueError(f"{fixture}:{rows.line_num}: {exc}") from exc
    if not cases:
        raise ValueError(f"{fixture}: no definition cases")
    return cases


def normalize_locations(result):
    from compare_definitions import path_from_uri

    if result is None:
        return []
    locations = result if isinstance(result, list) else [result]
    normalized = []
    for location in locations:
        if not isinstance(location, dict):
            raise ValueError(f"invalid definition location {location!r}")
        uri = location.get("targetUri", location.get("uri"))
        span = location.get("targetSelectionRange", location.get("range"))
        if not isinstance(uri, str) or not isinstance(span, dict):
            raise ValueError(f"invalid definition location {location!r}")
        for point in (span.get("start"), span.get("end")):
            if not isinstance(point, dict) or any(
                type(point.get(axis)) is not int or point[axis] < 0
                for axis in ("line", "character")
            ):
                raise ValueError(f"invalid definition range {span!r}")
        normalized.append({"path": str(path_from_uri(uri)), "range": span})
    return normalized


def classify_response(case: Case, result, *, target_line_offset: int = 0):
    """Classify the exact definition range, distinguishing GoLand package targets."""
    expected = {"kind": case.target_kind, "path": case.target_path}
    if case.target is not None:
        span = case.target.selection()
        for point in span.values():
            point["line"] += target_line_offset
        expected.update(path=str(case.target.path), range=span)

    record = {
        "id": case.id,
        "query": {"path": str(case.query.path), "position": case.position},
        "expected": expected,
        "raw_result": result,
    }
    try:
        locations = normalize_locations(result)
    except (OSError, ValueError, RuntimeError) as exc:
        return {**record, "status": "request-error", "error": str(exc), "locations": []}
    if case.target_kind == "package":
        status = "ide-package"
    elif case.target_kind == "empty":
        status = "unexpected" if locations else "expected-empty"
    elif not locations:
        status = "missing"
    elif (
        len(locations) == 1
        and Path(locations[0]["path"]) == Path(expected["path"])
        and locations[0]["range"] == expected["range"]
    ):
        status = "matched"
    else:
        status = "wrong"
    return {**record, "status": status, "locations": locations}


def check_cases(command, workspace, cases, timeout, *, method="definition"):
    from compare_definitions import LspClient

    client = LspClient(command, workspace, timeout)
    try:
        client.initialize()
        for path, text in {case.query.path: case.query.text for case in cases}.items():
            client.open_document(path, text)
        records = []
        for case in cases:
            try:
                result = client.request(
                    f"textDocument/{method}",
                    textDocument={"uri": case.query.path.as_uri()},
                    position=case.position,
                )
            except (OSError, RuntimeError) as exc:
                record = classify_response(case, None)
                record.update(status="request-error", error=str(exc))
            else:
                record = classify_response(case, result)
            records.append(record)
        client.shutdown()
        return records
    finally:
        client.close()


def describe(location):
    if not location:
        return "-"
    if "range" not in location:
        return location.get("path") or "-"
    start = location["range"]["start"]
    return f"{location['path']}:{start['line'] + 1}:{start['character'] + 1}"


def main():
    from compare_definitions import executable

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=executable)
    parser.add_argument("--workspace", required=True, type=Path)
    navigation_arguments(parser)
    parser.add_argument("--gomodcache", type=Path)
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--json", action="store_true", help="emit machine-readable results")
    parser.add_argument(
        "--allow-differences", action="store_true",
        help="report semantic differences without failing the command",
    )
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be positive and finite")

    try:
        workspace = args.workspace.resolve()
        fixture = navigation_fixture(args.method, args.fixture)
        cases = load_cases(fixture, workspace, args.gomodcache)
        records = check_cases([args.candidate], workspace, cases, args.timeout, method=args.method)
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as exc:
        print(f"GoLand comparison failed: {exc}", file=sys.stderr)
        return 1

    totals = Counter(record["status"] for record in records)
    if args.json:
        print(json.dumps({"summary": dict(totals), "cases": records}, indent=2))
    else:
        for record in records:
            actual = ", ".join(map(describe, record["locations"])) or "-"
            print(f"{record['id']}: {record['status']} | GoLand {describe(record['expected'])} | server {actual}")
            if "error" in record:
                print(f"  {record['error']}")
        print(", ".join(f"{status} {count}" for status, count in sorted(totals.items())))
    if args.allow_differences:
        return int(totals["request-error"] != 0)
    return int(any(totals[status] for status in ERROR_STATUSES))


if __name__ == "__main__":
    raise SystemExit(main())
