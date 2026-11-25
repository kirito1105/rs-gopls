#!/usr/bin/env python3
"""Compare definition performance against recorded GoLand targets on a real workspace."""

import argparse
from collections import Counter, defaultdict
import ctypes
from ctypes import wintypes
import hashlib
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import sys
import threading
import time

from check_goland_definitions import classify_response, load_cases
from compare_definitions import LspClient, executable


FIXTURE = Path(__file__).with_name("fixtures") / "localchat-goland.tsv"
DEFAULT_CASES = (
    "corsmiddleware", "inituser", "getsession", "interface-method", "config", "third-party-method",
)
PREFIX = "// rs-gopls benchmark overlay\n"
START = {"line": 0, "character": 0}
AFTER_PREFIX = {"line": 1, "character": 0}
MEMORY_INTERVAL_SECONDS = 0.05
GO_ENV_KEYS = (
    "GOVERSION", "GOROOT", "GOMODCACHE", "GOMOD", "GOWORK", "GOFLAGS",
    "GOOS", "GOARCH", "CGO_ENABLED", "GOPROXY",
)


class ProcessEntry(ctypes.Structure):
    _fields_ = (
        ("size", wintypes.DWORD), ("usage", wintypes.DWORD), ("pid", wintypes.DWORD),
        ("heap", ctypes.c_size_t), ("module", wintypes.DWORD), ("threads", wintypes.DWORD),
        ("parent_pid", wintypes.DWORD), ("priority", wintypes.LONG),
        ("flags", wintypes.DWORD), ("exe", wintypes.WCHAR * 260),
    )


class ProcessMemory(ctypes.Structure):
    _fields_ = (
        ("size", wintypes.DWORD), ("faults", wintypes.DWORD),
        ("peak_working_set", ctypes.c_size_t), ("working_set", ctypes.c_size_t),
        ("peak_paged_pool", ctypes.c_size_t), ("paged_pool", ctypes.c_size_t),
        ("peak_nonpaged_pool", ctypes.c_size_t), ("nonpaged_pool", ctypes.c_size_t),
        ("pagefile", ctypes.c_size_t), ("peak_pagefile", ctypes.c_size_t),
    )


class WorkingSetSampler:
    def __init__(self, pid):
        self.pid = pid
        self.parent_peak = None
        self.tree_peak = None
        self.samples = 0
        self.stopped = threading.Event()
        self.thread = None
        if sys.platform != "win32":
            return

        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        psapi = ctypes.WinDLL("psapi", use_last_error=True)
        self.snapshot = kernel.CreateToolhelp32Snapshot
        self.snapshot.argtypes = (wintypes.DWORD, wintypes.DWORD)
        self.snapshot.restype = wintypes.HANDLE
        self.first = kernel.Process32FirstW
        self.next = kernel.Process32NextW
        for get_entry in (self.first, self.next):
            get_entry.argtypes = (wintypes.HANDLE, ctypes.POINTER(ProcessEntry))
            get_entry.restype = wintypes.BOOL
        self.open = kernel.OpenProcess
        self.open.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
        self.open.restype = wintypes.HANDLE
        self.close = kernel.CloseHandle
        self.close.argtypes = (wintypes.HANDLE,)
        self.close.restype = wintypes.BOOL
        self.memory = psapi.GetProcessMemoryInfo
        self.memory.argtypes = (wintypes.HANDLE, ctypes.POINTER(ProcessMemory), wintypes.DWORD)
        self.memory.restype = wintypes.BOOL
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def working_set(self, pid):
        process_query_information = 0x0400
        process_vm_read = 0x0010
        handle = self.open(process_query_information | process_vm_read, False, pid)
        if not handle:
            return None
        try:
            size = ctypes.sizeof(ProcessMemory)
            counters = ProcessMemory(size=size)
            if self.memory(handle, ctypes.byref(counters), size):
                return counters.working_set, counters.peak_working_set
            return None
        finally:
            self.close(handle)

    def sample(self):
        root = self.working_set(self.pid)
        if root is None:
            return
        self.parent_peak = max(self.parent_peak or 0, root[1])

        snapshot_processes = 0x00000002
        handle = self.snapshot(snapshot_processes, 0)
        if handle is None or handle == ctypes.c_void_p(-1).value:
            return
        children = defaultdict(list)
        try:
            entry = ProcessEntry(size=ctypes.sizeof(ProcessEntry))
            ok = self.first(handle, ctypes.byref(entry))
            while ok:
                children[entry.parent_pid].append(entry.pid)
                ok = self.next(handle, ctypes.byref(entry))
        finally:
            self.close(handle)

        pending = [self.pid]
        visited = {self.pid}
        total = root[0]
        while pending:
            for child in children[pending.pop()]:
                if child in visited:
                    continue
                visited.add(child)
                pending.append(child)
                child_memory = self.working_set(child)
                if child_memory is not None:
                    total += child_memory[0]
        self.samples += 1
        self.tree_peak = max(self.tree_peak or 0, total)

    def run(self):
        while not self.stopped.is_set():
            self.sample()
            self.stopped.wait(MEMORY_INTERVAL_SECONDS)

    def finish(self):
        if self.thread is not None:
            self.stopped.set()
            self.thread.join()
            self.sample()
        return {
            "parent_peak_working_set_bytes": self.parent_peak,
            "process_tree_sampled_peak_working_set_bytes": self.tree_peak,
            "process_tree_sample_count": self.samples,
        }


def shifted_position(position, line_offset):
    return {"line": position["line"] + line_offset, "character": position["character"]}


def record_definition(samples, client, workspace, case, lane, started, *, query_offset=0, target_offset=0):
    position = shifted_position(case.position, query_offset)
    request_error = None
    try:
        response = client.request(
            "textDocument/definition",
            textDocument={"uri": case.query.path.as_uri()},
            position=position,
        )
    except (OSError, RuntimeError) as exc:
        response = None
        request_error = str(exc)
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    sample = {
        **classify_response(case, response, target_line_offset=target_offset),
        "lane": lane,
        "elapsed_ms": elapsed_ms,
        "query": {"path": case.query.path.relative_to(workspace).as_posix(), "position": position},
    }
    if request_error is not None:
        sample.update(status="request-error", error=request_error)
    samples.append(sample)
    if sample["status"] == "request-error":
        raise RuntimeError(sample["error"])


def run_session(command, workspace, cases, args, environment):
    result = {"samples": []}
    client = None
    memory = None
    try:
        start_case = cases[0]
        started = time.perf_counter_ns()
        client = LspClient([command], workspace, args.timeout, environment=environment)
        memory = WorkingSetSampler(client.process.pid)
        client.initialize()
        client.open_document(start_case.query.path, start_case.query.text)
        samples = result["samples"]
        record_definition(samples, client, workspace, start_case, "first-definition", started)

        versions = {start_case.query.path: 1}
        for case in cases[1:]:
            path = case.query.path
            if path not in versions:
                client.open_document(path, case.query.text)
                versions[path] = 1
            client.request(
                "textDocument/definition", textDocument={"uri": path.as_uri()}, position=case.position
            )

        edited = set()
        for index in range(args.iterations):
            case = cases[index % len(cases)]
            path = case.query.path
            versions[path] += 1
            if path in edited:
                edited.remove(path)
                end, text = AFTER_PREFIX, ""
            else:
                edited.add(path)
                end, text = START, PREFIX
            started = time.perf_counter_ns()
            client.change_document_range(path, versions[path], START, end, text)
            record_definition(
                samples, client, workspace, case, "caller-range-edit", started,
                query_offset=int(path in edited), target_offset=int(case.target.path in edited),
            )

        for path in versions:
            client.close_document(path)

        dependencies = [
            case for case in cases
            if case.target.path != case.query.path and case.target.path.is_relative_to(workspace)
        ]
        for index in range(args.dependency_iterations if dependencies else 0):
            case = dependencies[index % len(dependencies)]
            caller = case.query.path
            target = case.target.path
            client.open_document(caller, case.query.text)
            client.open_document(target, case.target.text)
            client.request(
                "textDocument/definition", textDocument={"uri": caller.as_uri()}, position=case.position
            )

            started = time.perf_counter_ns()
            client.change_document_range(target, 2, START, START, PREFIX)
            record_definition(
                samples, client, workspace, case, "dependency-overlay", started, target_offset=1,
            )

            started = time.perf_counter_ns()
            client.close_document(target)
            record_definition(samples, client, workspace, case, "dependency-restore", started)
            client.close_document(caller)

        result["memory"] = memory.finish()
        memory = None
        client.shutdown()
    except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
        result["error"] = str(exc)
        if client is not None:
            result["stderr"] = client.stderr_tail()
    finally:
        if memory is not None:
            result["memory"] = memory.finish()
        if client is not None:
            client.close()
    return result


def describe_binary(command, role, root, timeout):
    with Path(command).open("rb") as binary:
        digest = hashlib.file_digest(binary, "sha256").hexdigest()
    metadata = {"path": command, "sha256": digest}
    if role == "gopls":
        metadata["version"] = subprocess.check_output(
            [command, "version"], cwd=root, encoding="utf-8", timeout=timeout,
        ).strip()
    return metadata


def describe_sources(workspace, cases, timeout):
    try:
        revision = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=workspace, stderr=subprocess.DEVNULL,
            encoding="ascii", timeout=timeout,
        ).strip()
    except (OSError, subprocess.CalledProcessError):
        revision = None
    paths = {}
    for case in cases:
        paths[case.query.path.relative_to(workspace).as_posix()] = case.query.path
        paths[case.target_path] = case.target.path
    return {
        "revision": revision,
        "sha256": {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in paths.items()},
    }


def summarize(sessions, binary_roles):
    failed = {
        (sample["lane"], sample["id"])
        for session in sessions for sample in session["samples"] if sample["status"] != "matched"
    }
    complete = not any("error" in session for session in sessions)
    summary = {}
    for role in binary_roles:
        by_lane = defaultdict(list)
        statuses = Counter()
        for session in sessions:
            if session["server"] != role:
                continue
            for sample in session["samples"]:
                statuses[sample["status"]] += 1
                if complete and (sample["lane"], sample["id"]) not in failed:
                    by_lane[sample["lane"]].append(sample["elapsed_ms"])
        summary[role] = {
            "statuses": dict(statuses),
            "lanes": {
                lane: {
                    "samples": len(times),
                    "p50_ms": statistics.median(times),
                    "p95_ms": sorted(times)[math.ceil(0.95 * len(times)) - 1],
                }
                for lane, times in by_lane.items()
            },
        }
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=executable)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, default=FIXTURE)
    parser.add_argument("--prior", type=executable, help="path to the previous rs-gopls executable")
    parser.add_argument("--gopls", type=executable, default="gopls")
    parser.add_argument("--case", action="append", dest="case_ids", help="exact-target case (repeatable)")
    parser.add_argument("--sessions", type=int, default=5)
    parser.add_argument("--iterations", type=int, default=80, help="caller edits per session")
    parser.add_argument(
        "--dependency-iterations", type=int, help="overlay/restore cycles per session (default: --iterations)"
    )
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument("--json", type=Path, help="write per-request data, or use - for stdout")
    args = parser.parse_args()
    if args.dependency_iterations is None:
        args.dependency_iterations = args.iterations
    if min(args.sessions, args.iterations) < 1 or args.dependency_iterations < 0:
        parser.error("sessions and iterations must be positive; dependency iterations may be zero")
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("timeout must be finite and positive")

    try:
        workspace = args.workspace.resolve(strict=True)
        environment = {**os.environ, "GOPROXY": "off"}
        go_env = json.loads(subprocess.check_output(
            ["go", "env", "-json", *GO_ENV_KEYS], cwd=workspace, env=environment,
            encoding="utf-8", timeout=args.timeout,
        ))
        available = {
            case.id: case for case in load_cases(args.fixture, workspace, Path(go_env["GOMODCACHE"]))
            if case.target_kind == "file"
        }
        case_ids = list(dict.fromkeys(args.case_ids or DEFAULT_CASES))
        missing = [case_id for case_id in case_ids if case_id not in available]
        if missing:
            raise RuntimeError(f"cases lack pinned file targets: {', '.join(missing)}")
        cases = [available[case_id] for case_id in case_ids]

        commands = {"candidate": args.candidate}
        if args.prior:
            commands["prior"] = args.prior
        commands["gopls"] = args.gopls
        roles = list(commands)
        report = {
            "workspace": str(workspace),
            "fixture": str(args.fixture.resolve()),
            "fixture_sha256": hashlib.sha256(args.fixture.read_bytes()).hexdigest(),
            "sources": describe_sources(workspace, cases, args.timeout),
            "go": go_env,
            "workload": {
                "cases": case_ids,
                "sessions_per_server": args.sessions,
                "caller_range_edits_per_session": args.iterations,
                "dependency_overlay_cycles_per_session": args.dependency_iterations,
                "startup": "new processes with shared system and Go caches",
                "warm": "range edits in open Go files; no source writes",
                "gopls_diagnostics_trigger": "Save",
                "memory_sample_interval_ms": MEMORY_INTERVAL_SECONDS * 1000,
            },
            "binaries": {
                role: describe_binary(command, role, workspace, args.timeout)
                for role, command in commands.items()
            },
            "sessions": [],
        }
        for session in range(args.sessions):
            rotate = session % len(roles)
            for role in roles[rotate:] + roles[:rotate]:
                data = run_session(commands[role], workspace, cases, args, environment)
                report["sessions"].append({"server": role, "session": session + 1, **data})
                if "error" in data:
                    print(f"{role} session {session + 1}: {data['error']}", file=sys.stderr)

        report["summary"] = summarize(report["sessions"], roles)
        if args.json == Path("-"):
            print(json.dumps(report, ensure_ascii=False, indent=2))
        elif args.json:
            args.json.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

        output = sys.stderr if args.json == Path("-") else sys.stdout
        for role, data in report["summary"].items():
            print(f"{role} {data['statuses']}", file=output)
            for lane, timing in data["lanes"].items():
                print(
                    f"  {lane}: P50 {timing['p50_ms']:.2f} ms, P95 {timing['p95_ms']:.2f} ms "
                    f"({timing['samples']} samples)",
                    file=output,
                )
        return int(any(
            "error" in session or any(sample["status"] != "matched" for sample in session["samples"])
            for session in report["sessions"]
        ))
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as exc:
        print(f"Workspace benchmark failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
