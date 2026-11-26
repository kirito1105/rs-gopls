#!/usr/bin/env python3
"""Measure time from a go.mod save to the first correct cross-package definition."""

import argparse
from collections import Counter
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import sys
import tempfile
import time

from check_goland_definitions import normalize_locations
from compare_workspace_definitions import describe_binary
from lsp_harness import WORKSPACE_ENVIRONMENT, LspClient, executable, initialize_session, notify_changes, site, write


POLL_INTERVAL_SECONDS = 0.01


def module_config(replacement):
    return (
        "module example.test/graph-bench\n\ngo 1.22\n"
        "require example.test/dependency v0.0.0\n"
        f"replace example.test/dependency => ./versions/{replacement}\n"
    )


def setup(root):
    caller = write(
        root / "caller.go",
        'package graphbench\nimport "example.test/dependency"\n'
        "func Caller() { dependency.Run() }\n",
    )
    targets = {}
    for version in ("first", "second"):
        directory = root / "versions" / version
        write(directory / "go.mod", "module example.test/dependency\n\ngo 1.22\n")
        target = write(directory / "target.go", "package dependency\n\nfunc Run() {}\n")
        targets[version] = site(target, "func [[Run]]()")
    return site(caller, "dependency.[[Run]]()"), targets


def matches(locations, target):
    return (
        len(locations) == 1
        and Path(locations[0]["path"]) == target.path
        and locations[0]["range"] == target.selection()
    )


def await_target(client, reference, target, previous, started):
    deadline = started + client.timeout
    original_timeout = client.timeout
    retries = Counter()
    locations = []
    error = None
    try:
        while (remaining := deadline - time.monotonic()) > 0:
            client.timeout = remaining
            try:
                result = client.request(
                    "textDocument/definition",
                    textDocument={"uri": reference.path.as_uri()},
                    position=reference.cursor(),
                )
                locations = normalize_locations(result)
            except (OSError, RuntimeError, ValueError) as exc:
                error = str(exc)
                retries["request-error"] += 1
                break
            if matches(locations, target):
                return {
                    "status": "matched",
                    "elapsed_ms": (time.monotonic() - started) * 1000,
                    "retries": dict(retries),
                    "locations": locations,
                }
            if not locations:
                retries["missing"] += 1
            elif previous is not None and matches(locations, previous):
                retries["stale"] += 1
            else:
                retries["wrong"] += 1
            time.sleep(min(POLL_INTERVAL_SECONDS, max(0, deadline - time.monotonic())))
    finally:
        client.timeout = original_timeout
    return {
        "status": "failed",
        "elapsed_ms": (time.monotonic() - started) * 1000,
        "retries": dict(retries),
        "locations": locations,
        "error": error or "no exact target before the deadline",
    }


def measure(command, workspace, reference, targets, args, environment):
    report = {"samples": []}
    go_mod = write(workspace / "go.mod", module_config("first"))
    client = LspClient([command], workspace, args.timeout, environment=environment)
    try:
        initialize_session(client, watch_files=True, require_go_watches=False)
        client.open_document(reference.path, reference.text)

        current = "first"
        warm = await_target(client, reference, targets[current], None, time.monotonic())
        if warm["status"] != "matched":
            raise RuntimeError(f"failed to verify the initial graph: {warm!r}")
        for index in range(args.iterations):
            replacement = "second" if current == "first" else "first"
            started = time.monotonic()
            write(go_mod, module_config(replacement))
            notify_changes(client, (go_mod, 2))
            observed = await_target(client, reference, targets[replacement], targets[current], started)
            report["samples"].append({
                "iteration": index + 1,
                "replacement": replacement,
                "expected": {
                    "path": str(targets[replacement].path),
                    "range": targets[replacement].selection(),
                },
                **observed,
            })
            if observed["status"] != "matched":
                raise RuntimeError(f"failed to verify the {replacement} replacement: {observed!r}")
            current = replacement
        client.shutdown()
    except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
        report["error"] = str(exc)
        report["stderr"] = client.stderr_tail()
    finally:
        client.close()
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=executable)
    parser.add_argument("--gopls", type=executable, default="gopls")
    parser.add_argument("--sessions", type=int, default=5)
    parser.add_argument("--iterations", type=int, default=10, help="go.mod replacement edits per session")
    parser.add_argument("--timeout", type=float, default=30.0, help="seconds until each exact result")
    parser.add_argument("--json", type=Path, help="write per-save results to this file")
    args = parser.parse_args()
    if min(args.sessions, args.iterations) < 1 or not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("sessions, iterations and finite timeout must be positive")

    commands = [("candidate", args.candidate), ("gopls", args.gopls)]
    environment = {**os.environ, **WORKSPACE_ENVIRONMENT}
    report = {
        "workload": "temporary Go module; go.mod save and watched-file event to first exact target",
        "gopls_diagnostics_trigger": "Save",
        "retry_interval_ms": POLL_INTERVAL_SECONDS * 1000,
        "sessions": [],
    }
    try:
        with tempfile.TemporaryDirectory(prefix="rs-gopls-graph-bench-") as scratch:
            root = Path(scratch).resolve()
            reference, targets = setup(root)
            report["servers"] = {
                role: describe_binary(command, role, root, args.timeout)
                for role, command in commands
            }
            for index in range(args.sessions):
                order = commands if index % 2 == 0 else commands[::-1]
                for role, command in order:
                    sample = measure(command, root, reference, targets, args, environment)
                    report["sessions"].append({"server": role, "session": index + 1, **sample})
                    if "error" in sample:
                        print(f"{role} session {index + 1}: {sample['error']}", file=sys.stderr)

        if args.json:
            args.json.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        for role, _ in commands:
            samples = [
                sample for session in report["sessions"] if session["server"] == role
                for sample in session["samples"]
            ]
            times = sorted(sample["elapsed_ms"] for sample in samples if sample["status"] == "matched")
            retries = Counter()
            for sample in samples:
                retries.update(sample["retries"])
            planned = args.sessions * args.iterations
            timing = (
                f"P50 {statistics.median(times):.2f} ms, "
                f"P95 {times[math.ceil(0.95 * len(times)) - 1]:.2f} ms"
                if times else "no correct jumps"
            )
            print(f"{role}: {len(times)}/{planned} saves, {timing}, retries {dict(retries)}")
        return int(any("error" in session for session in report["sessions"]))
    except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
        print(f"Graph refresh benchmark failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
