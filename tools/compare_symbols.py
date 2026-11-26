#!/usr/bin/env python3
"""Compare GoLand and rs-gopls symbol latency, startup, and working sets."""

from __future__ import annotations

import argparse
from collections import Counter
from contextlib import contextmanager
from dataclasses import dataclass
import json
import math
import os
from pathlib import Path
import shutil
import subprocess
import sys
from tempfile import TemporaryDirectory
import time

from check_goland_symbols import (
    DOCUMENT, WORKSPACE, SymbolCorpus, go_environment, initialize_symbols,
)
from compare_definitions import LspClient, executable
from compare_hover_and_signature import percentile, sha256_file
from compare_workspace_definitions import MEMORY_INTERVAL_SECONDS, WorkingSetSampler


TOOLS = Path(__file__).resolve().parent
GOLAND = Path("E:/Apps/goland-2026.3.0.1.win/bin/goland64.exe")
START = {"line": 0, "character": 0}
READY_QUERY = "rsGoplsBenchmarkIndexReady"
DOCUMENT_LANE, WORKSPACE_LANE = "documentSymbol", "workspaceSymbol"


@dataclass(frozen=True)
class Case:
    request: str
    id: str
    edit_file: str
    query: str | None = None

    @property
    def key(self):
        return self.request, self.id

    def lsp_request(self, corpus):
        if self.request == DOCUMENT_LANE:
            return DOCUMENT, {"textDocument": {"uri": corpus.path(self.id).as_uri()}}
        return WORKSPACE, {"query": self.query}


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def elapsed_ms(started):
    return (time.perf_counter_ns() - started) / 1_000_000


def summarize(samples):
    return {
        "count": len(samples),
        "p50_ms": percentile(samples, 0.50),
        "p95_ms": percentile(samples, 0.95),
    }


def memory_result(sampler):
    measurements = sampler.finish()
    if not measurements["process_tree_sample_count"]:
        raise RuntimeError(f"working set sampler did not capture process {sampler.pid}")
    return {
        "pid": sampler.pid,
        "parent_peak_mib": round(measurements["parent_sampled_peak_working_set_bytes"] / 2**20, 2),
        "process_tree_peak_mib": round(measurements["process_tree_sampled_peak_working_set_bytes"] / 2**20, 2),
        "os_lifetime_parent_peak_mib": round(measurements["parent_peak_working_set_bytes"] / 2**20, 2),
        "samples": measurements["process_tree_sample_count"],
        "sampled_child_executables": measurements["sampled_child_executables"],
    }


def cases_for(corpus):
    return [
        *(Case(DOCUMENT_LANE, relative, relative) for relative in corpus.outlines),
        *(Case(WORKSPACE_LANE, case_id, row["editFile"], row["query"])
          for case_id, (row, _) in corpus.queries.items()),
    ]


def load_goland_bench(path, corpus, cases):
    bench = read_json(path)
    for key in ("schemaVersion", "project", "root", "revision", "build", "sdk",
                "scope", "context", "outlineMode", "editPrefix"):
        if bench[key] != corpus.data[key]:
            raise ValueError(f"{path} and symbol fixture disagree on {key}")
    case_timings = {(row["request"], row["id"]): row for row in bench["cases"]}
    if list(case_timings) != [case.key for case in cases] or len(case_timings) != len(bench["cases"]):
        raise ValueError("GoLand timings must cover the same cases in the captured order")
    warmup, iterations = bench["warmupRuns"], bench["sampleRuns"]
    if not isinstance(warmup, int) or not isinstance(iterations, int) or min(warmup, iterations) < 1:
        raise ValueError(f"invalid GoLand warmup/sample counts {warmup}/{iterations}")

    # When captures were repeated, pair the timings with the corresponding full
    # results and verify that the checked-in fixture is still the same corpus.
    timed_path = path.with_name(path.name.removesuffix("-bench.json") + "-timed.json")
    if not timed_path.is_file():
        timed_path = path.with_name(path.name.removesuffix("-bench.json") + ".json")
    timed = read_json(timed_path)
    captured_outlines = [(row["path"], row["structure"]) for row in corpus.data["outlines"]]
    if ([(row["path"], row["structure"]) for row in timed["outlines"]] != captured_outlines or
            timed["queries"] != corpus.data["queries"] or timed["sdk"] != bench["sdk"]):
        raise ValueError(f"{timed_path} does not match the checked-in GoLand navigation results")
    for case in cases:
        row = case_timings[case.key]
        expected = (corpus.outlines[case.id][0]["structure"] if case.request == DOCUMENT_LANE
                    else corpus.queries[case.id][0]["results"])
        if (row["query"], row["editFile"], row["resultCount"]) != (case.query, case.edit_file, len(expected)):
            raise ValueError(f"GoLand timing metadata does not match {case.key}")
        if len(row["timingsMs"]) != iterations or any(
            not math.isfinite(ms) or ms <= 0 for ms in (row["firstMs"], row["afterEditFirstMs"], *row["timingsMs"])
        ):
            raise ValueError(f"GoLand timings are incomplete for {case.key}")
    return bench, case_timings, timed_path


@contextmanager
def sampled_lsp(args, corpus, env):
    started = time.perf_counter_ns()
    client = LspClient([args.server], corpus.root, args.timeout, environment=env)
    sampler = WorkingSetSampler(client.process.pid)
    try:
        initialize_symbols(client, corpus, True)
        yield client, started, sampler
        client.shutdown()
    finally:
        sampler.finish()
        client.close()


def running_goland(launcher, pid):
    if pid is not None:
        return pid
    listing = subprocess.check_output([
        "powershell.exe", "-NoProfile", "-Command",
        "ConvertTo-Json -InputObject @(Get-Process goland64 -ErrorAction SilentlyContinue | Select-Object Id,Path) -Compress",
    ], encoding="utf-8")
    matches = [row["Id"] for row in json.loads(listing) if row["Path"] and Path(row["Path"]) == launcher]
    if len(matches) != 1:
        raise ValueError(f"expected one running {launcher.name} for reference sampling, found {matches}")
    return matches[0]


def measure_ready(args, corpus, cases, warmup, iterations, env, goland_pid):
    timings, baselines, differences = {}, {}, {}
    requests = {case.key: case.lsp_request(corpus) for case in cases}
    opened = set(corpus.data["sdk"]["openFiles"])
    versions = dict.fromkeys(opened, 1)
    prefix = corpus.data["editPrefix"]
    if not prefix.endswith("\n") or "\r" in prefix:
        raise ValueError("expected a GoLand buffer edit consisting of complete LF lines")
    lines_added = prefix.count("\n")

    def validate(case, result, delta=0):
        if case.request == DOCUMENT_LANE:
            corpus.check_document(case.id, result, lines=delta)
            if case.key not in differences:
                differences[case.key] = corpus.document_differences(case.id)
        else:
            actual = corpus.check_workspace(
                case.id, result, edited_file=case.edit_file, lines=delta, baseline=baselines.get(case.key),
            )
            if case.key not in baselines:
                baselines[case.key] = actual
                differences[case.key] = corpus.workspace_differences(case.id, actual)

    def measure(client, case, delta=0):
        method, params = requests[case.key]
        started = time.perf_counter_ns()
        result = client.request(method, **params)
        elapsed = elapsed_ms(started)
        validate(case, result, delta)
        return elapsed

    goland_sampler = WorkingSetSampler(goland_pid)
    try:
        with sampled_lsp(args, corpus, env) as (client, _, sampler):
            if client.request(WORKSPACE, query=READY_QUERY) != []:
                raise ValueError("the index readiness query unexpectedly matched project symbols")
            for case in cases:
                timings[case.key] = {"firstMs": measure(client, case)}
            for case in cases:
                method, params = requests[case.key]
                for _ in range(warmup):
                    validate(case, client.request(method, **params))
                timings[case.key]["timingsMs"] = [measure(client, case) for _ in range(iterations)]
                relative = case.edit_file
                path = corpus.path(relative)
                if relative not in opened:
                    client.open_document(path, corpus.source(relative).raw)
                    versions[relative] = 1
                versions[relative] += 1
                client.change_document_range(path, versions[relative], START, START, prefix)
                timings[case.key]["afterEditFirstMs"] = measure(client, case, lines_added)
                versions[relative] += 1
                client.change_document_range(
                    path, versions[relative], START, {"line": lines_added, "character": 0}, "",
                )
                if relative not in opened:
                    client.close_document(path)
                validate(case, client.request(method, **params))
            return timings, baselines, differences, memory_result(sampler), memory_result(goland_sampler)
    finally:
        goland_sampler.finish()


def copy_workspace(corpus, directory):
    workspace = directory / "workspace"
    if corpus.data["revision"]:
        tracked = subprocess.check_output(
            ["git", "ls-files", "-z", "--", "."], cwd=corpus.root,
        ).decode("utf-8").split("\0")
        for relative in filter(None, tracked):
            target = workspace / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(corpus.path(relative), target)
    else:
        shutil.copytree(
            corpus.root, workspace,
            ignore=shutil.ignore_patterns(".idea", "goland-symbols.json", "symbol-queries.json"),
        )
    spec = TOOLS / "fixtures" / "symbol-navigation" / "symbol-queries.json"
    shutil.copyfile(spec, directory / spec.name)
    return workspace


def prepare_goland(args, directory):
    config = directory / "config"
    config.mkdir()
    product = read_json(args.goland.parent.parent / "product-info.json")
    installed_config = Path(os.environ["APPDATA"]) / "JetBrains" / product["dataDirectoryName"]
    license_file = installed_config / "goland.key"
    if license_file.is_file():
        shutil.copyfile(license_file, config / license_file.name)
    properties = directory / "idea.properties"
    properties.write_text(
        "".join(f"idea.{key}.path={path.as_posix()}\n" for key, path in {
            "config": config, "system": directory / "system", "plugins": directory / "plugins",
            "log": directory / "log",
        }.items()) + "idea.initially.ask.config=false\nide.show.tips.on.startup.default.value=false\n",
        encoding="utf-8",
    )
    return properties


def cold_goland(args, corpus, case, directory, env, properties, label):
    control = directory / f"{label}.json"
    output = directory / f"{label}.json.result"
    error = directory / f"{label}.json.error"
    control.write_text(json.dumps({
        "project": corpus.project, "request": case.request, "id": case.id,
        "openFiles": corpus.data["sdk"]["openFiles"],
    }), encoding="utf-8")
    startup_info = subprocess.STARTUPINFO()
    startup_info.dwFlags |= subprocess.STARTF_USESHOWWINDOW
    startup_info.wShowWindow = subprocess.SW_HIDE
    environment = {**env, "GOLAND_PROPERTIES": str(properties), "RS_GOPLS_SYMBOL_STARTUP": str(control)}
    started = time.perf_counter_ns()
    process = subprocess.Popen(
        [str(args.goland), "ideScript", str(TOOLS / "capture_goland_symbols.groovy")],
        cwd=corpus.root, env=environment, startupinfo=startup_info,
    )
    sampler = WorkingSetSampler(process.pid)
    try:
        while not output.is_file():
            if error.is_file():
                raise RuntimeError(error.read_text(encoding="utf-8"))
            if process.poll() is not None or elapsed_ms(started) > args.timeout * 1000:
                log = directory / "log" / "idea.log"
                tail = log.read_text(encoding="utf-8", errors="replace").splitlines()[-12:] if log.is_file() else []
                raise RuntimeError(f"GoLand startup did not produce {case.key} (exit={process.poll()})\n" + "\n".join(tail))
            time.sleep(0.005)
        latency = elapsed_ms(started)
        memory = memory_result(sampler)
        result = read_json(output)
        if (result["pid"], result["build"], result["request"], result["id"]) != (
            process.pid, corpus.data["build"], case.request, case.id,
        ):
            raise ValueError("startup result was not produced by the newly created GoLand process")
        sdk = result["sdk"]
        if sdk != corpus.data["sdk"]:
            raise ValueError(f"GoLand startup settings differ from the timed capture: {sdk!r}")
        if case.request == DOCUMENT_LANE:
            expected = corpus.outlines[case.id][0]["structure"]
            if result["results"] != expected:
                raise ValueError(f"unexpected GoLand startup outline for {case.id}")
        else:
            captured = corpus.queries[case.id][0]["results"]
            if Counter(json.dumps(row, sort_keys=True) for row in result["results"]) != Counter(
                json.dumps(row, sort_keys=True) for row in captured
            ):
                raise ValueError(f"unexpected GoLand startup workspace results for {case.id}")
        process.wait(timeout=20)
        return {"elapsed_ms": latency, "working_set": memory}
    finally:
        sampler.finish()
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)


def measure_startup(args, corpus, common, baselines):
    selected = {
        DOCUMENT_LANE: next(case for case in common if case.request == DOCUMENT_LANE),
        WORKSPACE_LANE: next(case for case in common if case.request == WORKSPACE_LANE and case.query),
    }
    with TemporaryDirectory(prefix="rs-gopls-symbol-startup-") as name:
        directory = Path(name).resolve()
        workspace = copy_workspace(corpus, directory)
        copy = SymbolCorpus(workspace, args.fixture, check_revision=False)
        env, _ = go_environment(copy, args.timeout)
        properties = prepare_goland(args, directory)
        seed_case = selected[WORKSPACE_LANE]
        seed = cold_goland(args, copy, seed_case, directory, env, properties, "seed")
        startup = {"goland_empty_disk_index_seed": {"case": seed_case.id, **seed}}
        print(f"PASS {corpus.project} GoLand empty-disk-index seed", flush=True)
        for lane, case in selected.items():
            samples = {"rs_gopls": [], "goland": []}
            method, params = case.lsp_request(copy)
            for index in range(args.startup_runs):
                with sampled_lsp(args, copy, env) as (client, started, sampler):
                    result = client.request(method, **params)
                    latency = elapsed_ms(started)
                    memory = memory_result(sampler)
                    if lane == DOCUMENT_LANE:
                        copy.check_document(case.id, result)
                    else:
                        copy.check_workspace(case.id, result, baseline=baselines[case.key])
                    samples["rs_gopls"].append({"elapsed_ms": latency, "working_set": memory})
                label = f"{lane}-{index + 1}"
                samples["goland"].append(cold_goland(args, copy, case, directory, env, properties, label))
                print(f"PASS {corpus.project} startup {lane} {index + 1}/{args.startup_runs}", flush=True)
            startup[lane] = {
                "case": case.id,
                **{name: {"latency": summarize([sample["elapsed_ms"] for sample in rows]), "samples": rows}
                   for name, rows in samples.items()},
            }
    return startup


def latency_summary(cases, timings):
    return {
        "first": summarize([timings[case.key]["firstMs"] for case in cases]),
        "warm": summarize([ms for case in cases for ms in timings[case.key]["timingsMs"]]),
        "after_edit": summarize([timings[case.key]["afterEditFirstMs"] for case in cases]),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=executable)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--goland-bench", type=Path, required=True)
    parser.add_argument("--goland", type=Path, default=GOLAND)
    parser.add_argument("--goland-pid", type=int)
    parser.add_argument("--startup-runs", type=int, default=5)
    parser.add_argument("--timeout", type=float, default=90)
    parser.add_argument("--json", dest="json_out", type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != "win32":
        parser.error("the symbol benchmark uses the Windows GoLand and working set sampler")
    if args.startup_runs < 1 or not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--startup-runs and --timeout must be positive")
    try:
        args.goland = args.goland.resolve(strict=True)
        corpus = SymbolCorpus(args.workspace, args.fixture)
        if (args.goland.parent.parent / "build.txt").read_text().strip() != corpus.data["build"]:
            raise ValueError(f"{args.goland} does not match the GoLand capture build")
        env, go_version = go_environment(corpus, args.timeout)
        env["RS_GOPLS_BENCHMARK"] = "1"
        cases = cases_for(corpus)
        bench, goland_times, timed_path = load_goland_bench(args.goland_bench, corpus, cases)
        goland_pid = running_goland(args.goland, args.goland_pid)
        rs_times, baselines, differences, rs_memory, goland_memory = measure_ready(
            args, corpus, cases, bench["warmupRuns"], bench["sampleRuns"], env, goland_pid,
        )
        common = [case for case in cases if not differences[case.key]["reasons"]]
        startup = measure_startup(args, corpus, common, baselines)
        report = {
            "workspace": str(corpus.root), "revision": corpus.data["revision"], "go_version": go_version,
            "server": str(args.server), "server_sha256": sha256_file(args.server),
            "goland": str(args.goland), "goland_build": corpus.data["build"],
            "goland_timings": str(args.goland_bench.resolve()), "goland_results": str(timed_path.resolve()),
            "sdk": corpus.data["sdk"], "scope": corpus.data["scope"], "context": corpus.data["context"],
            "warmup_runs": bench["warmupRuns"], "sample_runs": bench["sampleRuns"],
            "edit_prefix": corpus.data["editPrefix"],
            "ready_conditions": {
                "rs_gopls": "test-inclusive package graph and symbol index ready; stdio request through JSON response",
                "goland": "smart mode; navigation call through full range extraction without UI rendering",
                "repeat": "same input after warmups; no on-disk edits; LSP document cache applies to open buffers",
            },
            "startup_conditions": "independent processes on the same temporary workspace copy. "
                                  "GoLand reuses the on-disk IDE caches of the seed launch; "
                                  "the LSP index starts empty; OS and Go disk caches are shared. "
                                  "GoLand completion is detected by polling an atomic results file every 5 ms.",
            "working_set": {
                "sample_interval_ms": MEMORY_INTERVAL_SECONDS * 1000,
                "rs_gopls_ready_session": rs_memory, "goland_concurrent_reference": goland_memory,
            },
            "startup": startup,
            "common": {
                lane: {
                    "case_ids": [case.id for case in common if case.request == lane],
                    "goland": latency_summary([case for case in common if case.request == lane], goland_times),
                    "rs_gopls": latency_summary([case for case in common if case.request == lane], rs_times),
                }
                for lane in (DOCUMENT_LANE, WORKSPACE_LANE)
            },
            "cases": [{
                "request": case.request, "id": case.id, "query": case.query, "edit_file": case.edit_file,
                "goland": goland_times[case.key], "rs_gopls": rs_times[case.key],
                "differences": differences[case.key],
            } for case in cases],
        }
        args.json_out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(f"PASS {corpus.project}: {len(common)}/{len(cases)} common cases, {len(cases) - len(common)} with differences")
        print(args.json_out.resolve())
    except (KeyError, OSError, RuntimeError, ValueError, subprocess.SubprocessError) as exc:
        print(f"FAIL {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
