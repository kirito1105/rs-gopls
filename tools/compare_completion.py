#!/usr/bin/env python3
"""Compare completion latency against captured GoLand BASIC timings."""

import argparse
from contextlib import ExitStack
from dataclasses import dataclass, field
import hashlib
import json
import math
from pathlib import Path
import platform
import subprocess
import sys

from check_goland_completion import (
    COMPLETION, METADATA, RESOLVE_PROPERTIES, CompletionCorpus, ExpectedCompletion, OpenBuffers,
    completion_arguments, completion_items, completion_session, read_json, validate_timeout,
    warm_package_graph, write_report,
)
from check_goland_symbols import go_environment
from compare_hover_and_signature import sha256_file
from compare_workspace_definitions import MEMORY_INTERVAL_SECONDS, WorkingSetSampler


PHASES = ("first", "warm", "after_edit")


def summarize(values):
    ordered = sorted(values)
    count = len(ordered)
    return {
        "count": count,
        "p50_ms": ordered[math.ceil(count * 0.50) - 1] if count else None,
        "p95_ms": ordered[math.ceil(count * 0.95) - 1] if count else None,
    }


def validate_indices(indices, count, label):
    if (not isinstance(indices, list) or any(type(i) is not int or not 0 <= i < count for i in indices)
            or sorted(set(indices)) != indices):
        raise ValueError(f"invalid GoLand {label} indices: {indices!r}")


def load_goland_bench(path, corpus):
    bench = read_json(path)
    for key in METADATA:
        if bench[key] != corpus.data[key]:
            raise ValueError(f"{path} and the completion fixture disagree on {key}")

    # The paired capture contains the candidate identities that the timing rows
    # describe only by query ID and result count.
    capture = None
    if path.name.endswith("-bench.json"):
        base = path.name.removesuffix("-bench.json")
        capture = next((sibling for suffix in ("-timed.json", ".json")
                        if (sibling := path.with_name(base + suffix)).is_file()), None)
        if capture and read_json(capture) != corpus.data:
            raise ValueError(f"GoLand results paired with {path} disagree with the completion fixture: {capture}")

    warmups, samples = bench["warmupRuns"], bench["sampleRuns"]
    if type(warmups) is not int or type(samples) is not int or min(warmups, samples) < 1:
        raise ValueError(f"invalid GoLand warmup/sample counts {warmups}/{samples}")
    cases = bench["cases"]
    if [row["id"] for row in cases] != list(corpus.queries):
        raise ValueError("GoLand timings must cover all completion queries in captured order")
    for row in cases:
        query = corpus.queries[row["id"]]
        if (row["path"], row["kind"], row["resultCount"]) != (
            query.row["path"], query.row["kind"], len(query.row["candidates"]),
        ):
            raise ValueError(f"GoLand timing metadata does not match {query.id}")
        repeat = row["timingsMs"]
        if len(repeat) != samples or any(
            type(ms) not in (int, float) or not math.isfinite(ms) or ms <= 0
            for ms in (row["firstMs"], row["afterEditFirstMs"], *repeat)
        ):
            raise ValueError(f"incomplete GoLand timings for {query.id}")
        validate_indices(row["rejectedWarmups"], warmups, f"{query.id} warmup")
        validate_indices(row["rejectedRepeatIndices"], samples, f"{query.id} repeat")
        if (bool(row["firstRejectedRepeatResult"]) != bool(row["rejectedRepeatIndices"]) or
                type(row["afterEditMatches"]) is not bool or row["afterEditMatches"] == bool(row["afterEditResult"]) or
                type(row["orderChanges"]) is not int or not 0 <= row["orderChanges"] <= samples - len(row["rejectedRepeatIndices"])):
            raise ValueError(f"inconsistent GoLand result checks for {query.id}")
        if "repeatVariants" in row:
            by_reason = {"order": [], "result": []}
            for variant in row["repeatVariants"]:
                indices = variant["indices"]
                validate_indices(indices, samples, f"{query.id} repeat variant")
                if not indices or not isinstance(variant["result"]["candidates"], list):
                    raise ValueError(f"invalid GoLand repeat variant for {query.id}")
                by_reason[variant["reason"]].extend(indices)
            variants = by_reason["order"] + by_reason["result"]
            if (len(variants) != len(set(variants))
                    or sorted(by_reason["result"]) != row["rejectedRepeatIndices"]
                    or len(by_reason["order"]) != row["orderChanges"]):
                raise ValueError(f"inconsistent GoLand repeat variants for {query.id}")
    return bench, capture


@dataclass
class CaseMeasurements:
    timings: dict
    matched_samples: list = field(default_factory=list)
    divergent_samples: list = field(default_factory=list)
    rs_gopls_results: list = field(default_factory=list)
    result_indexes: dict = field(default_factory=dict)
    warmup_errors: list = field(default_factory=list)

    def goland_sample(self, phase, index):
        row = self.timings
        if phase == "first":
            return row["firstMs"], "first", None
        if phase == "after_edit":
            if row["afterEditMatches"]:
                if "repeatVariants" not in row:
                    return row["afterEditFirstMs"], "after_edit_reference", "goland_after_edit_order_unverified"
                return row["afterEditFirstMs"], "after_edit_reference", None
            return row["afterEditFirstMs"], "after_edit_rejected", "goland_after_edit_result_changed"
        elapsed = row["timingsMs"][index]
        if "repeatVariants" in row:
            for variant_index, variant in enumerate(row["repeatVariants"]):
                if index in variant["indices"]:
                    return elapsed, f"repeat_variant_{variant_index}", f"goland_repeat_{variant['reason']}_changed"
            return elapsed, "first", None
        rejected = row["rejectedRepeatIndices"]
        if index in rejected:
            result = "first_rejected_repeat" if index == rejected[0] else None
            return elapsed, result, "goland_repeat_result_changed"
        if row["orderChanges"]:
            # The capture stores only the aggregate reorder count, so no other
            # warm sample for this query can be certified to have the same order.
            return elapsed, None, "goland_repeat_order_uncertain"
        return elapsed, "first", None

    def record(self, comparison, phase, index=None):
        goland_ms, goland_result, reason = self.goland_sample(phase, index)
        warmups_failed = phase != "first" and bool(self.warmup_errors)
        matched = comparison.matched and reason is None and not warmups_failed
        sample = {
            "phase": phase, "index": index, "matched": matched, "goland_ms": goland_ms,
            "rs_gopls_ms": comparison.completion_ms, "rs_gopls_resolve_ms": comparison.resolve_ms,
            "rs_gopls_failure": bool(comparison.failures) or warmups_failed,
        }
        if not matched:
            result = {"actual": comparison.rs_gopls, "failures": comparison.failures,
                      "intentional_divergences": comparison.intentional_divergences}
            digest = hashlib.sha256(json.dumps(result, ensure_ascii=False, separators=(",", ":")).encode("utf-8")).digest()
            if digest not in self.result_indexes:
                self.result_indexes[digest] = len(self.rs_gopls_results)
                self.rs_gopls_results.append(result)
            sample.update(goland_reason=reason, goland_result=goland_result,
                          rs_gopls_result=self.result_indexes[digest])
            if warmups_failed:
                sample["rs_gopls_reason"] = "rs_gopls_warmup_failed"
            self.divergent_samples.append(sample)
        else:
            self.matched_samples.append(sample)


def start_sampler(stack, pid):
    if pid is None or sys.platform != "win32":
        return None
    try:
        sampler = WorkingSetSampler(pid)
    except OSError:
        return None
    stack.callback(sampler.finish)
    return sampler


def memory(sampler):
    if sampler is None:
        return None
    value = sampler.finish()
    return {"pid": sampler.pid, **value} if value["process_tree_sample_count"] else None


def measure(args, corpus, bench, environment, report):
    cases = []
    with ExitStack() as stack:
        client, capabilities = stack.enter_context(completion_session(
            args.server, corpus.root, args.timeout, environment=environment,
        ))
        if not capabilities.get("completionProvider"):
            raise RuntimeError("server did not advertise completionProvider")
        rs_sampler = start_sampler(stack, client.process.pid)
        goland_sampler = start_sampler(stack, args.goland_pid)
        buffers = OpenBuffers(client, corpus)
        warm_package_graph(client)

        # Match the GoLand order: all first queries, followed by one transaction
        # per query containing the warmups, repeats, and first post-edit call.
        for query, row in zip(corpus.queries.values(), bench["cases"], strict=True):
            expected = ExpectedCompletion(corpus, query)
            edited = ExpectedCompletion(corpus, query, after_edit=True)
            results = {
                "first": expected.goland, "after_edit_reference": edited.goland,
                "first_rejected_repeat": row["firstRejectedRepeatResult"], "after_edit_rejected": row["afterEditResult"],
            }
            results.update({
                f"repeat_variant_{index}": variant["result"]
                for index, variant in enumerate(row.get("repeatVariants", ()))
            })
            measurements = CaseMeasurements(row)
            cases.append((expected, edited, measurements))
            report["cases"].append({
                "id": query.id, "kind": query.row["kind"], "path": query.row["path"],
                "position": expected.params["position"], "post_edit_position": edited.params["position"],
                "goland_results": results, "rs_gopls_results": measurements.rs_gopls_results,
                "matched_samples": measurements.matched_samples, "divergent_samples": measurements.divergent_samples,
                "goland_rejected_warmups": row["rejectedWarmups"],
                "goland_repeat_order_changes": row["orderChanges"],
                "rs_gopls_warmup_errors": measurements.warmup_errors,
            })
            with buffers.replay(query):
                measurements.record(expected.request(client), "first")
        for expected, edited, measurements in cases:
            with buffers.replay(expected.query):
                for index in range(bench["warmupRuns"]):
                    try:
                        completion_items(client.request(COMPLETION, **expected.params))
                    except (OSError, RuntimeError, ValueError) as exc:
                        measurements.warmup_errors.append({"index": index, "message": str(exc)})
                for index in range(bench["sampleRuns"]):
                    measurements.record(expected.request(client), "warm", index)
                with buffers.benchmark_edit(expected.query):
                    measurements.record(edited.request(client), "after_edit")
        report["working_set"] = {
            "interval_ms": MEMORY_INTERVAL_SECONDS * 1_000,
            "scope": "Windows parent and sampled descendants during the rs-gopls benchmark session",
            "rs_gopls": memory(rs_sampler), "goland_live_process": memory(goland_sampler),
        }


def common_summary(cases):
    summary = {}
    for phase in PHASES:
        matched = [sample for case in cases for sample in case["matched_samples"] if sample["phase"] == phase]
        divergent_count = sum(sample["phase"] == phase for case in cases for sample in case["divergent_samples"])
        verified = [
            sample for case in cases for sample in (*case["matched_samples"], *case["divergent_samples"])
            if sample["phase"] == phase and not sample["rs_gopls_failure"]
        ]
        summary[phase] = {
            "samples": len(matched) + divergent_count, "matched": len(matched), "divergent": divergent_count,
            "goland": summarize([sample["goland_ms"] for sample in matched]),
            "rs_gopls": summarize([sample["rs_gopls_ms"] for sample in matched]),
            "rs_gopls_resolve": summarize([
                sample["rs_gopls_resolve_ms"] for sample in matched if sample["rs_gopls_resolve_ms"] is not None
            ]),
            "rs_gopls_resolve_verified": summarize([
                sample["rs_gopls_resolve_ms"] for sample in verified if sample["rs_gopls_resolve_ms"] is not None
            ]),
        }
    return summary


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    completion_arguments(parser)
    parser.add_argument("--goland-bench", type=Path, required=True)
    parser.add_argument("--goland-pid", type=int, help="sample a running GoLand process on Windows")
    args = parser.parse_args(argv)
    validate_timeout(parser, args.timeout)
    if args.goland_pid is not None and args.goland_pid <= 0:
        parser.error("--goland-pid must be positive")
    try:
        corpus = CompletionCorpus(args.workspace, args.fixture)
        bench, paired = load_goland_bench(args.goland_bench, corpus)
        environment, version = go_environment(corpus, args.timeout)
        corpus.load_active_packages(environment, args.timeout)
        report = {
            "workspace": str(corpus.root), "revision": corpus.data["revision"], "fixture": str(corpus.fixture),
            "fixture_sha256": sha256_file(corpus.fixture), "host": platform.platform(),
            "server": args.server, "server_sha256": sha256_file(args.server), "go_version": version,
            "go_environment": {key: environment[key] for key in ("GOROOT", "GOOS", "GOARCH", "CGO_ENABLED", "GOWORK", "GOFLAGS")},
            "goland_build": bench["build"], "goland_bench": str(args.goland_bench.resolve()),
            "goland_paired_results": str(paired.resolve()) if paired is not None else None,
            "sdk": bench["sdk"], "warmup_runs": bench["warmupRuns"], "sample_runs": bench["sampleRuns"],
            "edit_prefix": bench["editPrefix"], "resolve_properties": RESOLVE_PROPERTIES,
            "conditions": {
                "goland": "Smart mode with stable indexes. BASIC invocation through sorted lookup and presentation "
                          "extraction. Popup painting, acceptance and buffer edits are outside the timing window.",
                "rs_gopls": "Initial editor buffers and test-inclusive package graph ready via workspace/symbol. "
                            "SDK completion index populated on demand. Each time spans sending a stdio JSON-RPC "
                            "request through reception and decoding of its response. Edits and comparison are outside timing.",
                "repeat": "Same unsaved buffer after the captured warmup count. A single session reuses its package "
                          "graph and indexes. Each completion is requested independently without a harness result cache.",
                "resolve": "Separate stdio timing for the captured acceptance target when resolution is needed. "
                           "Resolution runs outside completion timing and may populate caches reused by later calls. "
                           "The GoLand capture contains no counterpart resolution timings.",
                "goland_sample_evidence": "Stable samples have the same candidate identities, order and preselection "
                                          "as the first query, with UTF-16 ranges shifted after edits. Acceptance "
                                          "text comes from separate capture probes. Repeat variants record their "
                                          "indices and complete results.",
            },
            "cases": [],
        }
        try:
            measure(args, corpus, bench, environment, report)
        except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
            report["run_error"] = str(exc)
        report["common"] = common_summary(report["cases"])
        report["failed_samples"] = sum(
            sample["rs_gopls_failure"] for case in report["cases"] for sample in case["divergent_samples"]
        )
        if args.json_out:
            write_report(args.json_out, report)
        for phase, stats in report["common"].items():
            print(f"{phase}: {stats['matched']}/{stats['samples']} matched, {stats['divergent']} divergent")
            for side in ("goland", "rs_gopls", "rs_gopls_resolve", "rs_gopls_resolve_verified"):
                values = stats[side]
                if values["count"]:
                    print(f"  {side}: P50 {values['p50_ms']:.3f} ms, P95 {values['p95_ms']:.3f} ms "
                          f"({values['count']} samples)")
        if "run_error" in report:
            print(f"FAIL {report['run_error']}", file=sys.stderr)
            return 1
        if report["failed_samples"]:
            print(f"FAIL {report['failed_samples']} rs-gopls samples failed verification", file=sys.stderr)
            return 1
    except (KeyError, OSError, RuntimeError, TypeError, ValueError, subprocess.SubprocessError) as exc:
        print(f"FAIL {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
