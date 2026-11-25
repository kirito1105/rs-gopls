#!/usr/bin/env python3
"""Compare first implementation responses in fresh LSP processes."""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
import time

from check_goland_implementations import METHOD, initialize, load_cases, select_cases, selection_arguments
from compare_definitions import LspClient, executable
from compare_workspace_definitions import GO_ENV_KEYS, describe_binary
from compare_workspace_implementations import measure, print_results
from compare_workspace_references import successful, summary


def first_response(command, workspace, case, timeout, environment):
    session = {"samples": []}
    client = None
    try:
        started = time.perf_counter_ns()
        client = LspClient([command], workspace, timeout, environment=environment)
        initialize(client)
        client.open_document(case.query.path, case.query.text)
        session["samples"].append(measure(client, case, frozenset(), started, "first-correct"))
        client.shutdown()
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as exc:
        session["error"] = str(exc)
        if client is not None:
            session["stderr"] = client.stderr_tail()
    finally:
        if client is not None:
            client.close()
    return session


def run_case(commands, workspace, case, args, environment):
    sessions = []
    for index in range(args.sessions):
        offset = index % len(commands)
        for role, command in commands[offset:] + commands[:offset]:
            if role == "gopls" and args.empty_gopls_cache:
                with TemporaryDirectory(prefix="gopls-first-cache-") as directory:
                    session = first_response(
                        command, workspace, case, args.timeout, {**environment, "GOPLSCACHE": directory},
                    )
            else:
                session = first_response(command, workspace, case, args.timeout, environment)
            sessions.append({"server": role, "session": index + 1, **session})
    return sessions


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=executable, help="candidate language-server executable")
    selection_arguments(parser)
    parser.add_argument("--prior", type=executable, help="previous rs-gopls executable")
    parser.add_argument("--gopls", nargs="?", const="gopls", type=executable, help="include gopls (optionally supply its path)")
    parser.add_argument("--empty-gopls-cache", action="store_true", help="give each gopls process a fresh temporary cache")
    parser.add_argument("--sessions", type=int, default=5, help="new processes per server and case (default: 5)")
    parser.add_argument("--json", type=Path, help="write metadata, exact-match diagnostics and samples")
    args = parser.parse_args()
    if args.sessions < 1 or not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("sessions and finite timeout must be positive")
    if not args.prior and not args.gopls:
        try:
            args.gopls = executable("gopls")
        except argparse.ArgumentTypeError as exc:
            parser.error(str(exc))
    if args.empty_gopls_cache and not args.gopls:
        parser.error("--empty-gopls-cache requires a gopls comparison")

    try:
        workspace = args.workspace.resolve(strict=True)
        cases = select_cases(load_cases(args.fixture, workspace), args.case_ids, args.action)
        with_targets = [case for case in cases if case.expected()]
        if args.case_ids and len(with_targets) != len(cases):
            raise ValueError("each timed case must have a nonempty expected target set")
        cases = with_targets if args.case_ids else with_targets[:1]
        if not cases:
            raise ValueError("no nonempty GoLand case available for first-response timing")
        environment = {**os.environ, "GOPROXY": "off"}
        go_env = json.loads(subprocess.check_output(
            ["go", "env", "-json", *GO_ENV_KEYS], cwd=workspace, env=environment,
            encoding="utf-8", timeout=args.timeout,
        ))
        commands = [("candidate", args.candidate)]
        commands.extend((role, command) for role, command in (("prior", args.prior), ("gopls", args.gopls)) if command)
        report = {
            "workspace": str(workspace),
            "fixture": str(args.fixture.resolve()),
            "fixture_sha256": hashlib.sha256(args.fixture.read_bytes()).hexdigest(),
            "go": go_env,
            "workload": {
                "method": METHOD,
                "sessions_per_server_and_case": args.sessions,
                "timing": "process launch through first response, validated against the full expected target set",
                "startup": "new LSP processes with shared system and Go caches",
                "gopls_cache": ("empty per session" if args.empty_gopls_cache else "existing") if args.gopls else None,
                "gopls_diagnostics_trigger": "Save",
            },
            "binaries": {
                role: describe_binary(command, role, workspace, args.timeout) for role, command in commands
            },
            "cases": {},
        }
        passed = True
        for case in cases:
            print(f"{case.id} ({case.action})", flush=True)
            sessions = run_case(commands, workspace, case, args, environment)
            totals = summary(sessions)
            print_results(sessions, totals)
            report["cases"][case.id] = {"action": case.action, "sessions": sessions, "summary": totals}
            passed = all(map(successful, sessions)) and passed
        if args.json:
            args.json.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        return int(not passed)
    except (KeyError, OSError, RuntimeError, TypeError, ValueError, subprocess.SubprocessError) as exc:
        print(f"First implementation benchmark failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
