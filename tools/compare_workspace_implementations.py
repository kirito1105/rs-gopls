#!/usr/bin/env python3
"""Benchmark implementation navigation only on exact shared GoLand result sets."""

import argparse
import hashlib
import json
import math
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

from check_goland_implementations import (
    METHOD, check_cases, compare_response, display_result, initialize, load_cases,
    request_implementation, select_cases, selection_arguments,
)
from compare_definitions import LspClient, executable
from compare_workspace_definitions import (
    AFTER_PREFIX, GO_ENV_KEYS, PREFIX, START, WorkingSetSampler, describe_binary,
)
from compare_workspace_references import successful, summary


def measure(client, case, shifted_paths, started, lane):
    response = request_implementation(client, case, shifted_paths)
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    return {**compare_response(case, response, shifted_paths), "lane": lane, "elapsed_ms": elapsed_ms}


def run_session(command, workspace, cases, first, args, environment):
    session = {"samples": []}
    client = None
    memory = None
    try:
        started = time.perf_counter_ns()
        client = LspClient([command], workspace, args.timeout, environment=environment)
        memory = WorkingSetSampler(client.process.pid)
        initialize(client)
        client.open_document(first.query.path, first.query.text)
        session["samples"].append(measure(client, first, frozenset(), started, "first-correct"))
        versions = {first.query.path: 1}
        for case in cases:
            path = case.query.path
            if path not in versions:
                client.open_document(path, case.query.text)
                versions[path] = 1
        # Prime the full open-document view before measuring steady-state requests.
        for case in cases:
            response = request_implementation(client, case)
            check = compare_response(case, response)
            if check["status"] != "matched":
                session["preflight_difference"] = check
                raise ValueError(f"session preflight differs for {case.id}")

        shifted_paths = set()
        for index in range(args.iterations):
            case = cases[index % len(cases)]
            session["samples"].append(measure(
                client, case, shifted_paths, time.perf_counter_ns(), f"warm-{case.action}",
            ))

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
            started = time.perf_counter_ns()
            client.change_document_range(path, versions[path], START, end, text)
            session["samples"].append(measure(client, case, shifted_paths, started, f"edited-{case.action}"))

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


def print_results(sessions, totals):
    memory_key = "process_tree_sampled_peak_working_set_bytes"
    for session in sessions:
        if not successful(session):
            label = f"{session['server']} session {session['session']}"
            print(f"{label}: {session.get('error', 'timed results differ from GoLand')}", file=sys.stderr)
            if "preflight_difference" in session:
                display_result(session["preflight_difference"])
            for sample in session["samples"]:
                if sample["status"] != "matched":
                    display_result(sample)
    for role, data in totals.items():
        print(f"{role} {data['statuses']}")
        for lane, timing in data["lanes"].items():
            print(
                f"  {lane}: P50 {timing['p50_ms']:.2f} ms, P95 {timing['p95_ms']:.2f} ms "
                f"({timing['samples']} samples)"
            )
        peaks = [
            session["memory"][memory_key] / 1024**2
            for session in sessions
            if session["server"] == role and session.get("memory", {}).get(memory_key) is not None
        ]
        if peaks:
            print(f"  process tree working set: median peak {statistics.median(peaks):.1f} MiB, max {max(peaks):.1f} MiB")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=executable)
    selection_arguments(parser)
    parser.add_argument("--gopls", type=executable, default="gopls")
    parser.add_argument("--sessions", type=int, default=5)
    parser.add_argument("--iterations", type=int, default=80, help="per-session queries in each warm/edit lane")
    parser.add_argument("--json", type=Path, help="write metadata, differences and individual samples")
    args = parser.parse_args()
    if min(args.sessions, args.iterations) < 1 or not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("sessions, iterations and finite timeout must be positive")

    try:
        workspace = args.workspace.resolve(strict=True)
        cases = select_cases(load_cases(args.fixture, workspace), args.case_ids, args.action)
        environment = {**os.environ, "GOPROXY": "off"}
        go_env = json.loads(subprocess.check_output(
            ["go", "env", "-json", *GO_ENV_KEYS], cwd=workspace, env=environment,
            encoding="utf-8", timeout=args.timeout,
        ))
        commands = [("candidate", args.candidate), ("gopls", args.gopls)]
        preflight = {
            role: check_cases(command, workspace, cases, args.timeout, environment=environment)
            for role, command in commands
        }
        shared = [
            case for case, candidate, gopls in zip(cases, preflight["candidate"], preflight["gopls"], strict=True)
            if candidate["status"] == gopls["status"] == "matched"
        ]
        first = next((case for case in shared if case.expected()), None)
        report = {
            "workspace": str(workspace),
            "fixture": str(args.fixture.resolve()),
            "fixture_sha256": hashlib.sha256(args.fixture.read_bytes()).hexdigest(),
            "go": go_env,
            "workload": {
                "method": METHOD,
                "shared_cases": {case.id: case.action for case in shared},
                "first_case": first.id if first else None,
                "sessions_per_server": args.sessions,
                "warm_queries_per_session": args.iterations,
                "range_edits_per_session": args.iterations,
                "gopls_diagnostics_trigger": "Save",
                "startup": "new server processes with shared system and Go caches",
                "edits": "unsaved one-line insertions and deletions in query files",
            },
            "binaries": {
                role: describe_binary(command, role, workspace, args.timeout) for role, command in commands
            },
            "preflight": preflight,
            "sessions": [],
        }
        for role, results in preflight.items():
            different = [result for result in results if result["status"] != "matched"]
            print(f"{role}: {len(results) - len(different)}/{len(results)} exact GoLand cases")
            for result in results:
                if result["status"] != "matched" or result["exclusions"]:
                    display_result(result)
        candidate_passed = all(result["status"] == "matched" for result in preflight["candidate"])
        if candidate_passed and first is not None:
            for index in range(args.sessions):
                for role, command in commands if index % 2 == 0 else reversed(commands):
                    result = run_session(command, workspace, shared, first, args, environment)
                    report["sessions"].append({"server": role, "session": index + 1, **result})
        elif first is None:
            print("No common nonempty GoLand result was available for the first-response timer", file=sys.stderr)

        report["summary"] = summary(report["sessions"])
        if args.json:
            args.json.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print_results(report["sessions"], report["summary"])
        return int(not report["sessions"] or not all(map(successful, report["sessions"])))
    except (KeyError, OSError, RuntimeError, TypeError, ValueError, subprocess.SubprocessError) as exc:
        print(f"Implementation benchmark failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
