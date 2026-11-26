#!/usr/bin/env python3
"""Benchmark rs-gopls Hover and SignatureHelp against GoLand on localchat/server."""

import argparse
from dataclasses import replace
import hashlib
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import time

from check_goland_hover import check_hover, load_cases as load_hover_cases
from check_goland_signature_help import check_case as check_sig_case, load_cases as load_sig_cases
from compare_definitions import LspClient, executable
from compare_workspace_definitions import WorkingSetSampler

PREFIX = "// rs-gopls hover/signature benchmark overlay\n"
START = {"line": 0, "character": 0}
AFTER_PREFIX = {"line": 1, "character": 0}
EXCLUDED_HOVER_IDS = {"hover-pkg-qualifier", "hover-import-path", "hover-embedded-field-decl"}


def percentile(values, p):
    if not values:
        return None
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, math.ceil(len(ordered) * p) - 1))
    return ordered[index]


def summarize(values):
    if not values:
        return {"count": 0, "p50_ms": None, "p95_ms": None, "min_ms": None, "max_ms": None}
    return {
        "count": len(values),
        "p50_ms": round(statistics.median(values), 3),
        "p95_ms": round(percentile(values, 0.95), 3),
        "min_ms": round(min(values), 3),
        "max_ms": round(max(values), 3),
    }


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest().upper()


def make_env(go_bin):
    env = os.environ.copy()
    env.setdefault("GOWORK", "off")
    env.setdefault("GOPROXY", "off")
    if go_bin:
        path_key = next((k for k in env if k.upper() == "PATH"), "PATH")
        env[path_key] = str(go_bin) + os.pathsep + env.get(path_key, "")
    return env


def run_rs_gopls_session(server, workspace, hover_cases, sig_cases, sources, iterations, env):
    workspace = Path(workspace).resolve()
    t_start = time.perf_counter()
    client = LspClient([str(server)], workspace, 60.0, environment=env)
    sampler = WorkingSetSampler(client.process.pid)
    try:
        client.initialize()
        versions = {}
        for path, text in sorted(sources.items()):
            versions[path] = 1
            client.open_document(path, text)

        # Startup to first cross-package correct hover result
        anchor = next(c for c in hover_cases if c.id == "hover-cross-pkg-method-call")
        resp = client.request(
            "textDocument/hover",
            textDocument={"uri": anchor.path.as_uri()},
            position=anchor.position,
        )
        startup_ms = (time.perf_counter() - t_start) * 1000.0
        check_hover(anchor, resp, sources[anchor.path])

        # Ready-index new queries (first query per case in this session, excluding anchor)
        new_hover = []
        new_sig = []
        per_case_new = {}
        for c in hover_cases:
            t0 = time.perf_counter()
            res = client.request(
                "textDocument/hover",
                textDocument={"uri": c.path.as_uri()},
                position=c.position,
            )
            dt = (time.perf_counter() - t0) * 1000.0
            check_hover(c, res, sources[c.path])
            if c.id != anchor.id:
                new_hover.append(dt)
            per_case_new[c.id] = dt

        for c in sig_cases:
            t0 = time.perf_counter()
            res = client.request(
                "textDocument/signatureHelp",
                textDocument={"uri": c.path.as_uri()},
                position=c.position,
            )
            dt = (time.perf_counter() - t0) * 1000.0
            check_sig_case(c, res)
            new_sig.append(dt)
            per_case_new[c.id] = dt

        # Repeated warm queries
        rep_hover = []
        rep_sig = []
        per_case_rep = {c.id: [] for c in list(hover_cases) + list(sig_cases)}
        for _ in range(iterations):
            for c in hover_cases:
                t0 = time.perf_counter()
                res = client.request(
                    "textDocument/hover",
                    textDocument={"uri": c.path.as_uri()},
                    position=c.position,
                )
                dt = (time.perf_counter() - t0) * 1000.0
                check_hover(c, res, sources[c.path])
                rep_hover.append(dt)
                per_case_rep[c.id].append(dt)
            for c in sig_cases:
                t0 = time.perf_counter()
                res = client.request(
                    "textDocument/signatureHelp",
                    textDocument={"uri": c.path.as_uri()},
                    position=c.position,
                )
                dt = (time.perf_counter() - t0) * 1000.0
                check_sig_case(c, res)
                rep_sig.append(dt)
                per_case_rep[c.id].append(dt)

        # Post-edit queries (insert/remove top-level comment line, verify shifted result)
        edit_hover = []
        edit_sig = []
        edit_hover_cases = [
            c for c in hover_cases if c.id in (
                "hover-func-decl-doc",
                "hover-cross-pkg-method-call",
                "hover-struct-field-tag",
                "hover-local-inferred",
            )
        ]
        edit_sig_cases = [
            c for c in sig_cases if c.id in (
                "sig-iface-method-p1",
                "sig-cross-pkg-func-p2",
                "sig-stdlib-variadic-p1",
                "sig-nested-inner",
            )
        ]
        inserted = set()
        for _ in range(iterations):
            for c in edit_hover_cases:
                versions[c.path] += 1
                if c.path in inserted:
                    client.change_document_range(c.path, versions[c.path], START, AFTER_PREFIX, "")
                    inserted.remove(c.path)
                    shift = 0
                    text = sources[c.path]
                else:
                    client.change_document_range(c.path, versions[c.path], START, START, PREFIX)
                    inserted.add(c.path)
                    shift = 1
                    text = PREFIX + sources[c.path]
                shifted = replace(
                    c,
                    position={"line": c.position["line"] + shift, "character": c.position["character"]},
                    token_range={
                        "start": {"line": c.token_range["start"]["line"] + shift, "character": c.token_range["start"]["character"]},
                        "end": {"line": c.token_range["end"]["line"] + shift, "character": c.token_range["end"]["character"]},
                    },
                )
                t0 = time.perf_counter()
                res = client.request(
                    "textDocument/hover",
                    textDocument={"uri": c.path.as_uri()},
                    position=shifted.position,
                )
                dt = (time.perf_counter() - t0) * 1000.0
                check_hover(shifted, res, text)
                edit_hover.append(dt)

            for c in edit_sig_cases:
                versions[c.path] += 1
                if c.path in inserted:
                    client.change_document_range(c.path, versions[c.path], START, AFTER_PREFIX, "")
                    inserted.remove(c.path)
                    shift = 0
                else:
                    client.change_document_range(c.path, versions[c.path], START, START, PREFIX)
                    inserted.add(c.path)
                    shift = 1
                shifted = replace(
                    c,
                    position={"line": c.position["line"] + shift, "character": c.position["character"]},
                )
                t0 = time.perf_counter()
                res = client.request(
                    "textDocument/signatureHelp",
                    textDocument={"uri": c.path.as_uri()},
                    position=shifted.position,
                )
                dt = (time.perf_counter() - t0) * 1000.0
                check_sig_case(shifted, res)
                edit_sig.append(dt)

        mem = sampler.finish()
        parent_peak = mem["parent_peak_working_set_bytes"]
        tree_peak = mem["process_tree_sampled_peak_working_set_bytes"]
        return {
            "startup_ms": startup_ms,
            "new_hover": new_hover,
            "new_sig": new_sig,
            "rep_hover": rep_hover,
            "rep_sig": rep_sig,
            "edit_hover": edit_hover,
            "edit_sig": edit_sig,
            "per_case_new": per_case_new,
            "per_case_rep": per_case_rep,
            "parent_peak_mib": round(parent_peak / (1024 * 1024), 2) if parent_peak else None,
            "tree_peak_mib": round(tree_peak / (1024 * 1024), 2) if tree_peak else None,
            "memory_samples": mem["process_tree_sample_count"],
        }
    finally:
        sampler.finish()
        client.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("server")
    parser.add_argument("--workspace", type=Path, default=Path(r"E:\Code\localchat\server"))
    parser.add_argument("--fixture", type=Path, default=Path(r"tools\fixtures\localchat-goland-hover-signature.json"))
    parser.add_argument("--goland-bench", type=Path)
    parser.add_argument("--go-bin", type=Path, default=Path(r"C:\Users\rick\go\pkg\mod\golang.org\toolchain@v0.0.1-go1.27.4.windows-amd64\bin"))
    parser.add_argument("--sessions", type=int, default=5)
    parser.add_argument("--iterations", type=int, default=30)
    parser.add_argument("--json", dest="json_out", type=Path)
    args = parser.parse_args()

    server = executable(args.server)
    all_hover, hover_sources = load_hover_cases(args.fixture, args.workspace)
    all_sig, sig_sources = load_sig_cases(args.fixture, args.workspace)
    hover_cases = [c for c in all_hover if c.id not in EXCLUDED_HOVER_IDS]
    sig_cases = list(all_sig)
    sources = {**hover_sources, **sig_sources}

    env = make_env(args.go_bin if args.go_bin.is_dir() else None)
    go_exe = str(args.go_bin / "go.exe") if (args.go_bin / "go.exe").is_file() else "go"
    go_ver = subprocess.check_output([go_exe, "version"], env=env, text=True).strip()

    startups = []
    all_new_h, all_new_s = [], []
    all_rep_h, all_rep_s = [], []
    all_edit_h, all_edit_s = [], []
    parent_peaks, tree_peaks = [], []
    per_case_new = {c.id: [] for c in hover_cases + sig_cases}
    per_case_rep = {c.id: [] for c in hover_cases + sig_cases}

    for _ in range(args.sessions):
        r = run_rs_gopls_session(server, args.workspace, hover_cases, sig_cases, sources, args.iterations, env)
        startups.append(r["startup_ms"])
        all_new_h.extend(r["new_hover"])
        all_new_s.extend(r["new_sig"])
        all_rep_h.extend(r["rep_hover"])
        all_rep_s.extend(r["rep_sig"])
        all_edit_h.extend(r["edit_hover"])
        all_edit_s.extend(r["edit_sig"])
        if r["parent_peak_mib"] is not None:
            parent_peaks.append(r["parent_peak_mib"])
        if r["tree_peak_mib"] is not None:
            tree_peaks.append(r["tree_peak_mib"])
        for cid, v in r["per_case_new"].items():
            per_case_new[cid].append(v)
        for cid, vs in r["per_case_rep"].items():
            per_case_rep[cid].extend(vs)

    report = {
        "server": str(server),
        "server_sha256": sha256_file(server),
        "workspace": str(args.workspace.resolve()),
        "go_version": go_ver,
        "sessions": args.sessions,
        "iterations": args.iterations,
        "common_hover_cases": len(hover_cases),
        "common_signature_cases": len(sig_cases),
        "excluded_hover_ids": sorted(EXCLUDED_HOVER_IDS),
        "rs_gopls": {
            "startup_first_hover": summarize(startups),
            "new_hover": summarize(all_new_h),
            "new_signature": summarize(all_new_s),
            "repeated_hover": summarize(all_rep_h),
            "repeated_signature": summarize(all_rep_s),
            "edited_hover": summarize(all_edit_h),
            "edited_signature": summarize(all_edit_s),
            "parent_peak_mib": {"median": round(statistics.median(parent_peaks), 2), "max": round(max(parent_peaks), 2)} if parent_peaks else None,
            "tree_peak_mib": {"median": round(statistics.median(tree_peaks), 2), "max": round(max(tree_peaks), 2)} if tree_peaks else None,
            "per_case_new": {cid: summarize(vs) for cid, vs in per_case_new.items()},
            "per_case_rep": {cid: summarize(vs) for cid, vs in per_case_rep.items()},
        },
    }

    if args.goland_bench and args.goland_bench.is_file():
        gl = json.loads(args.goland_bench.read_text(encoding="utf-8"))
        gl_new_h, gl_new_s, gl_rep_h, gl_rep_s = [], [], [], []
        gl_per_case = {}
        for c in gl["cases"]:
            if c["id"] in EXCLUDED_HOVER_IDS:
                continue
            if c["kind"] == "hover":
                if c.get("firstMs") is not None:
                    gl_new_h.append(c["firstMs"])
                gl_rep_h.extend(c.get("timingsMs", []))
            else:
                if c.get("firstMs") is not None:
                    gl_new_s.append(c["firstMs"])
                gl_rep_s.extend(c.get("timingsMs", []))
            gl_per_case[c["id"]] = {
                "first_ms": round(c["firstMs"], 3) if c.get("firstMs") is not None else None,
                "repeated": summarize(c.get("timingsMs", [])),
            }
        report["goland"] = {
            "build": gl.get("build"),
            "sdk": gl.get("sdk"),
            "first_hover": summarize(gl_new_h),
            "first_signature": summarize(gl_new_s),
            "repeated_hover": summarize(gl_rep_h),
            "repeated_signature": summarize(gl_rep_s),
            "per_case": gl_per_case,
        }

    text = json.dumps(report, ensure_ascii=False, indent=2)
    if args.json_out:
        args.json_out.write_text(text + "\n", encoding="utf-8")
    print(text)


if __name__ == "__main__":
    main()
