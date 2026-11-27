"""Run the GoLand processor capture in a trusted temporary copy, then verify its output."""

import argparse
import base64
from collections import defaultdict
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid

from replay import CASES, HERE, SOURCE, changes, expected, position_at, snapshot, spans
from lsp_harness import WORKSPACE_ENVIRONMENT


def prepare(root, cases):
    temp = Path(tempfile.gettempdir()).resolve()
    if root.parent.parent != temp or root.name != "workspace" or not root.parent.name.startswith("rs-gopls-rename-ide-"):
        raise ValueError("choose a temporary workspace created by replay.py copy")
    files = snapshot(SOURCE)
    if snapshot(root) != files:
        raise ValueError("the temporary workspace must match the initial fixture")
    job = temp / "rs-gopls-rename-captures" / uuid.uuid4().hex
    job.mkdir(parents=True)
    (job / "root.txt").write_text(str(root), encoding="utf-8")
    manifest = "".join(f"{name}\t{hashlib.sha256(data).hexdigest()}\n" for name, data in files.items())
    (job / "files.tsv").write_text(manifest, encoding="utf-8")
    requests = []
    for case in cases:
        _, text, start, end = spans(root, case)
        pos = position_at(text, start)
        requests.append("\t".join(map(str, [
            case["id"], case["path"], pos["line"], pos["character"], text[start:end], case["new"]
        ])))
    (job / "cases.tsv").write_text("\n".join(requests) + "\n", encoding="utf-8")
    script = (HERE / "capture.groovy").read_text(encoding="utf-8")
    (job / "capture.groovy").write_text(script.replace("__RENAME_JOB__", job.name), encoding="utf-8")
    return job


def collect(job, cases):
    pristine = snapshot(SOURCE)
    report = {"build": (job / "build.txt").read_text().strip(), "cases": []}
    for case in cases:
        directory = job / case["id"]
        fields = defaultdict(list)
        for row in (directory / "observations.tsv").read_text(encoding="utf-8").splitlines():
            key, value = row.split("\t", maxsplit=1)
            fields[key].append(base64.b64decode(value, validate=True).decode("utf-8"))
        [outcome] = fields.pop("outcome")
        actual = {**pristine, **snapshot(directory / "changed")}
        for filename in fields.pop("deleted", []):
            actual.pop(filename)
        row = {
            "case": case["id"], "outcome": outcome, **fields,
            "changes": list(changes(pristine, actual)),
            "matchesPlannedFiles": actual == expected(case),
        }
        if outcome == "applied":
            with tempfile.TemporaryDirectory(prefix="rs-gopls-rename-observed-") as temp:
                root = Path(temp).resolve()
                for name, data in actual.items():
                    path = (root / name).resolve()
                    if not path.is_relative_to(root):
                        raise ValueError(f"invalid captured file: {name}")
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(data)
                result = subprocess.run(["go", "test", "./..."], cwd=root, timeout=90,
                                        env={**os.environ, **WORKSPACE_ENVIRONMENT},
                                        capture_output=True, text=True)
                row["goTest"] = {"exitCode": result.returncode, "output": result.stdout + result.stderr}
        report["cases"].append(row)
        print(case["id"], outcome, flush=True)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=lambda p: Path(p).resolve())
    parser.add_argument("--goland", type=Path, required=True)
    parser.add_argument("--case", choices=CASES)
    parser.add_argument("--timeout", type=int, default=600)
    parser.add_argument("--prepare-only", action="store_true", help="stage a request without launching the IDE")
    args = parser.parse_args()
    cases = [CASES[args.case]] if args.case else list(CASES.values())
    job = prepare(args.root, cases)
    print(job, flush=True)
    if args.prepare_only:
        return
    deadline = time.monotonic() + args.timeout
    subprocess.run([str(args.goland.resolve()), "ideScript", str(job / "capture.groovy")],
                   check=True, timeout=args.timeout)
    while not (job / "done").is_file():
        if (job / "failed.txt").exists():
            sys.exit((job / "failed.txt").read_text(encoding="utf-8"))
        if time.monotonic() > deadline:
            sys.exit(f"No completion marker in {job}. Inspect the IDE dialog and log.")
        time.sleep(1)
    report = collect(job, cases)
    if snapshot(args.root) != snapshot(SOURCE):
        raise ValueError("IDE output captured, but temporary workspace was not restored")
    output = job / "goland-rename.json"
    output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(output)
    if any(row["outcome"] == "unavailable" for row in report["cases"]):
        sys.exit("At least one case could not run. See the captured errors.")


if __name__ == "__main__":
    main()
