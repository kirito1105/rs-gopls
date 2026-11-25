#!/usr/bin/env python3
"""Check implementation navigation against exact captured GoLand locations."""

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
from check_goland_definitions import fixture_path, go_path, normalize_locations, query_site, utf16_offset
from check_goland_references import Location, describe_location
from compare_definitions import LspClient, executable


DEFAULT_FIXTURE = Path(__file__).resolve().parent / "fixtures" / "localchat-goland-implementations.json"
METHOD = "textDocument/implementation"
ACTIONS = {"implementation": "implementations", "super": "super"}


@dataclass(frozen=True)
class Case:
    id: str
    action: str
    query: Site
    position: dict[str, int]
    observed: tuple[Location, ...]
    exclusions: dict[Location, str]

    def expected(self, shifted_paths=frozenset()):
        return {location.shifted(shifted_paths) for location in self.observed if location not in self.exclusions}

    def excluded_records(self):
        return [{**location.record(), "reason": reason} for location, reason in self.exclusions.items()]


def load_cases(fixture, workspace):
    """Read captured selections and verify that their tokens still match the source."""
    corpus = json.loads(fixture.read_text(encoding="utf-8-sig"))
    roots = {"": workspace.resolve()}
    texts = {}
    cases = {}

    def source(path):
        if path not in texts:
            texts[path] = path.read_text(encoding="utf-8")
        return texts[path]

    def location(item):
        prefix, separator, relative = item["path"].partition(":")
        if separator:
            if prefix not in ("GOMODCACHE", "GOROOT"):
                raise ValueError(f"unknown Go path prefix {prefix!r}")
            if prefix not in roots:
                roots[prefix] = go_path(workspace, prefix)
            path = fixture_path(relative, roots[prefix])
        else:
            path = fixture_path(prefix, roots[""])
        target = Location.from_range(path, item["range"])
        text = source(path)
        start = utf16_offset(text, *target.start)
        end = utf16_offset(text, *target.end)
        if text[start:end] != item["token"]:
            raise ValueError(f"{path}:{target.start} does not select {item['token']!r}")
        return target

    for item in corpus["cases"]:
        case_id = item["id"]
        try:
            if not case_id or case_id in cases:
                raise ValueError("empty or duplicate case id")
            action = item["action"]
            query = item["query"]
            path = fixture_path(query["path"], roots[""])
            position = {axis: query[axis] for axis in ("line", "character")}
            if any(type(value) is not int for value in position.values()):
                raise ValueError(f"invalid query position {position!r}")
            origin = query_site(path, source(path), position, query["token"])
            observations = {}
            for key in ACTIONS.values():
                targets = tuple(map(location, item[key]))
                if len(targets) != len(set(targets)):
                    raise ValueError(f"duplicate locations in GoLand {key}")
                observations[key] = targets
            observed = observations[ACTIONS[action]]
            exclusions = {}
            for excluded in item.get("exclusions", ()):
                target = location(excluded)
                reason = excluded["reason"]
                if target not in observed or target in exclusions or not isinstance(reason, str) or not reason.strip():
                    raise ValueError(f"invalid GoLand location exclusion {excluded!r}")
                exclusions[target] = reason
            cases[case_id] = Case(case_id, action, origin, position, observed, exclusions)
        except (KeyError, OSError, TypeError, ValueError) as exc:
            raise ValueError(f"{fixture} case {case_id!r}: {exc}") from exc
    if not cases:
        raise ValueError(f"{fixture}: no implementation cases")
    return cases


def select_cases(cases, ids=None, action=None):
    if ids:
        for case_id in ids:
            if case_id not in cases:
                raise ValueError(f"unknown implementation case {case_id!r}")
        cases = {case_id: cases[case_id] for case_id in ids}
    selected = [case for case in cases.values() if action is None or case.action == action]
    if not selected:
        raise ValueError("no implementation cases selected")
    return selected


def initialize(client):
    capabilities = client.initialize()
    provider = capabilities.get("implementationProvider")
    if provider is not True and not isinstance(provider, dict):
        raise RuntimeError("server did not advertise implementationProvider")


def request_implementation(client, case, shifted_paths=frozenset()):
    position = case.position
    if case.query.path in shifted_paths:
        position = {**position, "line": position["line"] + 1}
    return client.request(METHOD, textDocument={"uri": case.query.path.as_uri()}, position=position)


def compare_response(case, response, shifted_paths=frozenset()):
    expected = case.expected(shifted_paths)
    counts = Counter(
        Location.from_range(Path(item["path"]), item["range"])
        for item in normalize_locations(response)
    )
    missing = sorted(expected - counts.keys())
    extra = sorted(counts.keys() - expected)
    duplicates = sorted(location for location, count in counts.items() if count > 1)
    return {
        "case": case.id,
        "action": case.action,
        "status": "different" if missing or extra or duplicates else "matched",
        "expected_count": len(expected),
        "returned_count": counts.total(),
        "observed_count": len(case.observed),
        "exclusions": case.excluded_records(),
        "missing": [location.record() for location in missing],
        "extra": [location.record() for location in extra],
        "duplicates": [{**location.record(), "count": counts[location]} for location in duplicates],
    }


def check_cases(command, workspace, cases, timeout, *, environment=None):
    client = LspClient([command], workspace, timeout, environment=environment)
    try:
        initialize(client)
        for path, text in {case.query.path: case.query.text for case in cases}.items():
            client.open_document(path, text)
        results = []
        for case in cases:
            try:
                results.append(compare_response(case, request_implementation(client, case)))
            except (OSError, ValueError, RuntimeError) as exc:
                results.append({
                    "case": case.id, "action": case.action, "status": "request-error",
                    "observed_count": len(case.observed), "exclusions": case.excluded_records(),
                    "error": str(exc),
                })
        client.shutdown()
        return results
    finally:
        client.close()


def display_result(item):
    label = f"{item['case']} ({item['action']})"
    if "error" in item:
        print(f"{label}: {item['error']}")
    else:
        print(f"{label}: {item['status']} ({item['returned_count']}/{item['expected_count']})")
        for status in ("missing", "extra", "duplicates"):
            for location in item[status]:
                print(f"  {status}: {describe_location(location)}")
    for location in item["exclusions"]:
        print(f"  excluded GoLand location: {describe_location(location)} ({location['reason']})")


def selection_arguments(parser):
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, default=DEFAULT_FIXTURE)
    parser.add_argument("--case", action="append", dest="case_ids", help="captured case ID (repeatable)")
    parser.add_argument("--action", choices=ACTIONS, help="select GoLand implementation or super cases")
    parser.add_argument("--timeout", type=float, default=60.0)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=executable)
    selection_arguments(parser)
    parser.add_argument("--json", action="store_true", help="emit machine-readable results")
    parser.add_argument("--allow-differences", action="store_true", help="report differences without failing")
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be positive and finite")

    try:
        workspace = args.workspace.resolve(strict=True)
        cases = select_cases(load_cases(args.fixture, workspace), args.case_ids, args.action)
        results = check_cases(args.candidate, workspace, cases, args.timeout)
    except (KeyError, OSError, RuntimeError, TypeError, ValueError, subprocess.SubprocessError) as exc:
        print(f"GoLand implementation comparison failed: {exc}", file=sys.stderr)
        return 1

    summary = Counter(item["status"] for item in results)
    if args.json:
        print(json.dumps({"summary": dict(summary), "cases": results}, ensure_ascii=False, indent=2))
    else:
        for result in results:
            display_result(result)
        print(", ".join(f"{status} {count}" for status, count in sorted(summary.items())))
    return int(bool(summary["request-error"] or (summary["different"] and not args.allow_differences)))


if __name__ == "__main__":
    raise SystemExit(main())
