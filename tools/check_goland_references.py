#!/usr/bin/env python3
"""Check semantic reference sets against the captured GoLand usages."""

from __future__ import annotations

import argparse
from collections import Counter
from dataclasses import dataclass
import json
import math
from pathlib import Path
import subprocess
import sys

from check_definitions import Site
from check_goland_definitions import fixture_path, normalize_locations, query_site, utf16_offset
from compare_definitions import LspClient, executable


DEFAULT_FIXTURE = Path(__file__).resolve().parent / "fixtures" / "localchat-goland-references.json"


@dataclass(frozen=True, order=True)
class Location:
    path: Path
    start: tuple[int, int]
    end: tuple[int, int]

    def shifted(self, paths):
        if self.path not in paths:
            return self
        return Location(
            self.path,
            (self.start[0] + 1, self.start[1]),
            (self.end[0] + 1, self.end[1]),
        )

    def record(self):
        def point(value):
            return {"line": value[0], "character": value[1]}

        return {"path": str(self.path), "range": {"start": point(self.start), "end": point(self.end)}}

    @classmethod
    def from_range(cls, path, span):
        points = []
        for field in ("start", "end"):
            point = span[field]
            if any(type(point[axis]) is not int or point[axis] < 0 for axis in ("line", "character")):
                raise ValueError(f"invalid position in {span!r}")
            points.append((point["line"], point["character"]))
        start, end = points
        if end <= start:
            raise ValueError(f"invalid reference range {span!r}")
        return cls(path, start, end)


@dataclass(frozen=True)
class Case:
    id: str
    query: Site
    position: dict[str, int]
    declaration: Location
    usages: tuple[Location, ...]
    ide_usage_count: int

    def expected(self, include_declaration, shifted_paths):
        locations = (*self.usages, self.declaration) if include_declaration else self.usages
        return {location.shifted(shifted_paths) for location in locations}


def load_cases(fixture, workspace):
    """Read the captured source ranges and verify each token against the workspace."""
    corpus = json.loads(fixture.read_text(encoding="utf-8-sig"))
    workspace = workspace.resolve()
    texts = {}
    cases = {}

    def source(path):
        if path not in texts:
            texts[path] = path.read_text(encoding="utf-8")
        return texts[path]

    def location(item):
        path = fixture_path(item["path"], workspace)
        result = Location.from_range(path, item["range"])
        text = source(path)
        start = utf16_offset(text, *result.start)
        end = utf16_offset(text, *result.end)
        if text[start:end] != item["token"]:
            raise ValueError(f"{path}:{result.start} does not select {item['token']!r}")
        return result

    for item in corpus["cases"]:
        case_id = item["id"]
        try:
            if not case_id or case_id in cases:
                raise ValueError("empty or duplicate case id")
            query = item["query"]
            path = fixture_path(query["path"], workspace)
            position = {axis: query[axis] for axis in ("line", "character")}
            if any(type(value) is not int for value in position.values()):
                raise ValueError(f"invalid query position {position!r}")
            origin = query_site(path, source(path), position, query["token"])
            declaration = location(item["declaration"])
            usages = tuple(map(location, item["usages"]))
            ide_usages = tuple(map(location, item["ide_usages"]))
            locations = (declaration, *usages, *ide_usages)
            if len(locations) != len(set(locations)):
                raise ValueError("duplicate locations in the captured GoLand case")
            cases[case_id] = Case(case_id, origin, position, declaration, usages, len(ide_usages))
        except (KeyError, OSError, TypeError, ValueError) as exc:
            raise ValueError(f"{fixture} case {case_id!r}: {exc}") from exc
    if not cases:
        raise ValueError(f"{fixture}: no reference cases")
    return cases


def select_cases(cases, ids):
    if not ids:
        return list(cases.values())
    included = []
    for case_id in dict.fromkeys(ids):
        if case_id not in cases:
            raise ValueError(f"unknown reference case {case_id!r}")
        included.append(cases[case_id])
    return included


def request_references(client, case, include_declaration, shifted_paths=frozenset()):
    position = case.position
    if case.query.path in shifted_paths:
        position = {**position, "line": position["line"] + 1}
    return client.request(
        "textDocument/references",
        textDocument={"uri": case.query.path.as_uri()},
        position=position,
        context={"includeDeclaration": include_declaration},
    )


def compare_response(case, response, include_declaration, shifted_paths=frozenset()):
    expected = case.expected(include_declaration, shifted_paths)
    if response is not None and not isinstance(response, list):
        raise ValueError("references response must be a list of locations or null")
    counts = Counter(
        Location.from_range(Path(item["path"]), item["range"])
        for item in normalize_locations(response)
    )
    missing = sorted(expected - counts.keys())
    extra = sorted(counts.keys() - expected)
    duplicates = sorted(location for location, count in counts.items() if count > 1)
    return {
        "case": case.id,
        "include_declaration": include_declaration,
        "status": "different" if missing or extra or duplicates else "matched",
        "expected_count": len(expected),
        "returned_count": sum(counts.values()),
        "ide_usage_count": case.ide_usage_count,
        "missing": [location.record() for location in missing],
        "extra": [location.record() for location in extra],
        "duplicates": [{**location.record(), "count": counts[location]} for location in duplicates],
    }


def check_cases(command, workspace, cases, timeout):
    client = LspClient([command], workspace, timeout)
    try:
        client.initialize()
        for path, text in {case.query.path: case.query.text for case in cases}.items():
            client.open_document(path, text)
        results = []
        for case in cases:
            for include_declaration in (False, True):
                try:
                    response = request_references(client, case, include_declaration)
                    results.append(compare_response(case, response, include_declaration))
                except (OSError, ValueError, RuntimeError) as exc:
                    results.append({
                        "case": case.id, "include_declaration": include_declaration,
                        "status": "request-error", "error": str(exc),
                    })
        client.shutdown()
        return results
    finally:
        client.close()


def describe_location(location):
    start = location["range"]["start"]
    end = location["range"]["end"]
    return (
        f"{location['path']}:{start['line'] + 1}:{start['character'] + 1}"
        f"-{end['line'] + 1}:{end['character'] + 1}"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=executable)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, default=DEFAULT_FIXTURE)
    parser.add_argument("--case", action="append", dest="case_ids", help="captured case ID (repeatable)")
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--json", action="store_true", help="emit machine-readable results")
    parser.add_argument("--allow-differences", action="store_true", help="report differences without failing")
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be positive and finite")

    try:
        workspace = args.workspace.resolve()
        cases = select_cases(load_cases(args.fixture, workspace), args.case_ids)
        results = check_cases(args.candidate, workspace, cases, args.timeout)
    except (KeyError, OSError, RuntimeError, TypeError, ValueError, subprocess.SubprocessError) as exc:
        print(f"GoLand reference comparison failed: {exc}", file=sys.stderr)
        return 1

    summary = Counter(item["status"] for item in results)
    if args.json:
        print(json.dumps({"summary": dict(summary), "cases": results}, ensure_ascii=False, indent=2))
    else:
        for item in results:
            label = f"{item['case']} include_declaration={item['include_declaration']}"
            if "error" in item:
                print(f"{label}: {item['error']}")
                continue
            print(f"{label}: {item['status']} ({item['returned_count']}/{item['expected_count']})")
            for status in ("missing", "extra", "duplicates"):
                for location in item[status]:
                    print(f"  {status}: {describe_location(location)}")
        print(", ".join(f"{status} {count}" for status, count in sorted(summary.items())))
    return int(summary["request-error"] != 0 or (summary["different"] != 0 and not args.allow_differences))


if __name__ == "__main__":
    raise SystemExit(main())
