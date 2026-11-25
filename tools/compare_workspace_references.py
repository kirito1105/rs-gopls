#!/usr/bin/env python3
"""Benchmark semantic Find Usages against identical GoLand reference sets."""

import argparse
from collections import Counter, defaultdict
import hashlib
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import sys
import time

from check_goland_references import (
    DEFAULT_FIXTURE, compare_response, load_cases, request_references, select_cases,
)
from compare_definitions import LspClient, executable
from compare_workspace_definitions import (
    AFTER_PREFIX, GO_ENV_KEYS, PREFIX, START, WorkingSetSampler, describe_binary,
)


GROUPS = {
    "local": ("local-exec-path", "local-auth-header", "local-session-variable"),
    "same-package": (
        "same-file-cors", "same-package-getmux", "same-package-getmux-use",
        "multi-file-route-field", "same-package-struct-field-and-test",
        "same-package-struct-field-use-test",
    ),
    "cross-package": (
        "cross-package-getsession", "cross-package-getsession-use", "cross-package-type",
        "cross-package-type-use", "concrete-package-method", "interface-method",
        "empty-concrete-method", "distinct-method-collision",
    ),
}
CASE_GROUPS = {case_id: group for group, ids in GROUPS.items() for case_id in ids}
DEFAULT_CASES = (
    "cross-package-getsession", "local-exec-path", "same-package-getmux",
    "cross-package-type", "local-auth-header", "multi-file-route-field",
    "concrete-package-method", "local-session-variable", "same-package-struct-field-and-test",
)


def measure(client, case, include_declaration, shifted_paths, started, lane):
    response = request_references(client, case, include_declaration, shifted_paths)
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    check = compare_response(case, response, include_declaration, shifted_paths)
    return {**check, "lane": lane, "elapsed_ms": elapsed_ms}


def run_session(command, workspace, cases, args, environment):
    session = {"samples": [], "preflight": []}
    client = None
    memory = None
    try:
        first_case = cases[0]
        started = time.perf_counter_ns()
        client = LspClient([command], workspace, args.timeout, environment=environment)
        memory = WorkingSetSampler(client.process.pid)
        client.initialize()
        client.open_document(first_case.query.path, first_case.query.text)
        session["samples"].append(
            measure(client, first_case, False, frozenset(), started, "first-reference")
        )
        versions = {first_case.query.path: 1}
        for case in cases:
            path = case.query.path
            if path not in versions:
                client.open_document(path, case.query.text)
                versions[path] = 1
            for include_declaration in (False, True):
                response = request_references(client, case, include_declaration)
                session["preflight"].append(compare_response(case, response, include_declaration))
        if any(check["status"] != "matched" for check in session["preflight"]):
            raise ValueError("preflight did not match the GoLand reference set")

        shifted_paths = set()
        for index in range(args.iterations):
            case = cases[index % len(cases)]
            path = case.query.path
            versions[path] += 1
            if path in shifted_paths:
                shifted_paths.remove(path)
                end, text = AFTER_PREFIX, ""
            else:
                shifted_paths.add(path)
                end, text = START, PREFIX
            include_declaration = bool((index // len(cases)) % 2)
            started = time.perf_counter_ns()
            client.change_document_range(path, versions[path], START, end, text)
            session["samples"].append(measure(
                client, case, include_declaration, shifted_paths, started,
                CASE_GROUPS.get(case.id, "other"),
            ))

        session["memory"] = memory.finish()
        memory = None
        client.shutdown()
    except (OSError, RuntimeError, ValueError) as exc:
        session["error"] = str(exc)
        if client is not None:
            session["stderr"] = client.stderr_tail()
    finally:
        if memory is not None:
            session["memory"] = memory.finish()
        if client is not None:
            client.close()
    return session


def successful(session):
    return "error" not in session and all(item["status"] == "matched" for item in session["samples"])


def summary(sessions):
    comparable = all(map(successful, sessions))
    result = {}
    for role in dict.fromkeys(session["server"] for session in sessions):
        counts = Counter()
        by_lane = defaultdict(list)
        for session in sessions:
            if session["server"] != role:
                continue
            for sample in session["samples"]:
                counts[sample["status"]] += 1
                if comparable:
                    by_lane[sample["lane"]].append(sample["elapsed_ms"])
        result[role] = {
            "statuses": dict(counts),
            "lanes": {
                lane: {
                    "samples": len(times),
                    "p50_ms": statistics.median(times),
                    "p95_ms": sorted(times)[math.ceil(0.95 * len(times)) - 1],
                }
                for lane, times in by_lane.items()
            },
        }
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=executable)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, default=DEFAULT_FIXTURE)
    comparison = parser.add_mutually_exclusive_group()
    comparison.add_argument("--gopls", type=executable, help="gopls executable (default: gopls on PATH)")
    comparison.add_argument("--prior", type=executable, help="compare against a previous rs-gopls release")
    parser.add_argument("--case", action="append", dest="case_ids", help="shared exact-set case (repeatable)")
    parser.add_argument("--sessions", type=int, default=5)
    parser.add_argument("--iterations", type=int, default=80, help="caller range edits per session")
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--json", type=Path, help="write metadata, individual samples and exact-set diagnostics")
    args = parser.parse_args()
    if min(args.sessions, args.iterations) < 1 or not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("sessions, iterations and finite timeout must be positive")
    if not args.prior and not args.gopls:
        try:
            args.gopls = executable("gopls")
        except argparse.ArgumentTypeError as exc:
            parser.error(str(exc))

    try:
        workspace = args.workspace.resolve(strict=True)
        cases = select_cases(load_cases(args.fixture, workspace), args.case_ids or DEFAULT_CASES)
        environment = {**os.environ, "GOPROXY": "off"}
        go_env = json.loads(subprocess.check_output(
            ["go", "env", "-json", *GO_ENV_KEYS], cwd=workspace, env=environment,
            encoding="utf-8", timeout=args.timeout,
        ))
        comparison_server = ("prior", args.prior) if args.prior else ("gopls", args.gopls)
        commands = [("candidate", args.candidate), comparison_server]
        report = {
            "workspace": str(workspace),
            "fixture": str(args.fixture.resolve()),
            "fixture_sha256": hashlib.sha256(args.fixture.read_bytes()).hexdigest(),
            "go": go_env,
            "workload": {
                "method": "textDocument/references",
                "cases": {case.id: CASE_GROUPS.get(case.id, "other") for case in cases},
                "sessions_per_server": args.sessions,
                "range_edits_per_session": args.iterations,
                "gopls_diagnostics_trigger": "Save",
                "startup": "new server processes with shared system and Go caches",
                "warm": "unsaved one-line insertions and deletions, with both includeDeclaration settings",
            },
            "binaries": {
                role: describe_binary(command, role, workspace, args.timeout)
                for role, command in commands
            },
            "sessions": [],
        }
        for index in range(args.sessions):
            order = commands if index % 2 == 0 else commands[::-1]
            for role, command in order:
                result = run_session(command, workspace, cases, args, environment)
                report["sessions"].append({"server": role, "session": index + 1, **result})
                if not successful(result):
                    problems = [
                        f"{item['case']} ({item['include_declaration']})"
                        for item in result["preflight"] + result["samples"] if item["status"] != "matched"
                    ]
                    details = result.get("error", "range-edit results differ from GoLand")
                    print(f"{role} session {index + 1}: {details} {', '.join(problems)}", file=sys.stderr)

        report["summary"] = summary(report["sessions"])
        if args.json:
            args.json.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        for role, data in report["summary"].items():
            print(f"{role} {data['statuses']}")
            for lane, timing in data["lanes"].items():
                print(
                    f"  {lane}: P50 {timing['p50_ms']:.2f} ms, P95 {timing['p95_ms']:.2f} ms "
                    f"({timing['samples']} samples)"
                )
        return int(not all(map(successful, report["sessions"])))
    except (KeyError, OSError, RuntimeError, TypeError, ValueError, subprocess.SubprocessError) as exc:
        print(f"Reference benchmark failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
