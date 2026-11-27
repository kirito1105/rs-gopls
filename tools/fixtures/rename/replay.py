"""Compare GoLand or rs-gopls output against a copy of the same Go workspace."""

import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[1]))
from lsp_harness import LspClient, WORKSPACE_ENVIRONMENT, executable, path_from_uri, position_at

SOURCE = HERE / "workspace"
SPEC = json.loads((HERE / "cases.json").read_text(encoding="utf-8"))
CASES = {case["id"]: case for case in SPEC["cases"]}
MARKED = re.compile(r"\[\[([^\[\]]*)\]\]")


def snapshot(root):
    return {
        path.relative_to(root).as_posix(): path.read_bytes()
        for path in sorted(root.rglob("*"))
        if path.is_file() and not {".idea", ".git"}.intersection(path.relative_to(root).parts)
    }


def ranges(text, anchor):
    matches = list(MARKED.finditer(anchor))
    bare = MARKED.sub(r"\1", anchor)
    if not matches or text.count(bare) != 1:
        raise ValueError(f"expected a unique marked anchor: {anchor!r}")
    start = text.index(bare)
    return [(start + m.start() - index * 4, start + m.end() - (index + 1) * 4)
            for index, m in enumerate(matches)]


def spans(root, case):
    path = root / case["path"]
    text = path.read_bytes().decode("utf-8")
    anchor = case["anchor"].replace("\n", "\r\n") if "\r\n" in text else case["anchor"]
    [(start, end)] = ranges(text, anchor)
    return path, text, start, end


def expected(case):
    files = snapshot(SOURCE)
    for name, anchors in SPEC["edits"].get(case["edits"], {}).items():
        text = files[name].decode("utf-8")
        offsets = []
        for anchor in anchors:
            if "\r\n" in text:
                anchor = anchor.replace("\n", "\r\n")
            offsets.extend(ranges(text, anchor))
        for start, end in sorted(offsets, reverse=True):
            replacement = case["new"] if start != end else case["new"] + " "
            text = text[:start] + replacement + text[end:]
        files[name] = text.encode("utf-8")
    return files


def changes(before, after):
    for name in sorted(before.keys() | after.keys()):
        a, b = before.get(name), after.get(name)
        if a != b:
            yield {
                "path": name,
                "beforeSha256": hashlib.sha256(a).hexdigest() if a is not None else None,
                "afterSha256": hashlib.sha256(b).hexdigest() if b is not None else None,
                "diff": "".join(difflib.unified_diff(
                    (a or b"").decode().splitlines(keepends=True),
                    (b or b"").decode().splitlines(keepends=True),
                    fromfile="a/" + name, tofile="b/" + name,
                )),
            }


def go_test(root, timeout=90):
    subprocess.run(["go", "test", "./..."], cwd=root, check=True, timeout=timeout,
                   env={**os.environ, **WORKSPACE_ENVIRONMENT})


def utf16_offset(text, pos):
    lines = text.split("\n")
    line, col = pos["line"], pos["character"]
    if not 0 <= line < len(lines) or not 0 <= col <= len(lines[line].rstrip("\r").encode("utf-16-le")) // 2:
        raise ValueError(f"out-of-bounds UTF-16 position: {pos}")
    prefix = lines[line].encode("utf-16-le")[:2 * col].decode("utf-16-le")
    return sum(len(item) + 1 for item in lines[:line]) + len(prefix)


def apply_workspace_edit(root, opened, edit):
    if "changes" in edit or not isinstance(edit.get("documentChanges"), list):
        raise ValueError(f"expected versioned documentChanges: {edit}")
    updates = {}
    for group in edit["documentChanges"]:
        doc = group["textDocument"]
        path = path_from_uri(doc["uri"])
        if not path.is_relative_to(root) or path in updates:
            raise ValueError(f"invalid or duplicate edit group: {doc}")
        if path in opened:
            version, text = opened[path]
        else:
            version, text = None, path.read_bytes().decode("utf-8")
        if doc["version"] != version:
            raise ValueError(f"version mismatch: {doc}")
        entries = []
        for item in group["edits"]:
            span = item["range"]
            start, end = (utf16_offset(text, span[side]) for side in ("start", "end"))
            entries.append((start, end, item["newText"]))
        boundary = len(text)
        for start, end, new in sorted(entries, reverse=True):
            if start > end or end > boundary:
                raise ValueError(f"overlapping or reversed edits for {path}")
            text = text[:start] + new + text[end:]
            boundary = start
        updates[path] = text.encode("utf-8")
    for path, contents in updates.items():
        path.write_bytes(contents)


def replay(server, root, case, timeout=90):
    path, text, start, end = spans(root, case)
    client = LspClient([server], root, timeout)
    try:
        capabilities = client.initialize(document_changes=True, prepare_rename=True)
        if capabilities.get("renameProvider", {}) != {"prepareProvider": True}:
            raise ValueError("server did not advertise prepareRename")
        client.open_document(path, text)
        at = {"textDocument": {"uri": path.as_uri()}, "position": position_at(text, start)}
        prep = client.request("textDocument/prepareRename", **at)
        span = {"start": position_at(text, start), "end": position_at(text, end)}
        if prep != {"range": span, "placeholder": text[start:end]}:
            raise ValueError(f"unexpected prepareRename result: {prep}")
        response = client.wait_response(client.start_request("textDocument/rename", **at, newName=case["new"]))
        if case["edits"] is None:
            if not response.get("error", {}).get("message"):
                raise ValueError(f"expected conflict: {response}")
        else:
            if "error" in response:
                raise ValueError(f"unexpected rename rejection: {response['error']}")
            apply_workspace_edit(root, {path: (1, text)}, response["result"])
        client.shutdown()
    finally:
        client.close()
    mismatch = list(changes(expected(case), snapshot(root)))
    if mismatch:
        raise ValueError(f"{case['id']} differs from expected:\n" + "".join(c["diff"] for c in mismatch))
    if case["edits"] is not None:
        go_test(root, timeout)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("copy", help="create an isolated copy and print its path")
    for command in ("cursor", "record"):
        option = sub.add_parser(command)
        option.add_argument("root", type=Path)
        option.add_argument("case", choices=CASES)
        if command == "record":
            option.add_argument("--outcome", choices=["applied", "conflict", "unavailable"], required=True)
            option.add_argument("--observation", required=True, help="observed IDE message and range choice")
    for command in ("check", "lsp"):
        option = sub.add_parser(command)
        if command == "lsp":
            option.add_argument("server", type=executable)
        option.add_argument("--case", choices=CASES)
    args = parser.parse_args()
    if args.command == "copy":
        print(shutil.copytree(SOURCE, Path(tempfile.mkdtemp(prefix="rs-gopls-rename-ide-")) / "workspace"))
    elif args.command == "cursor":
        case = CASES[args.case]
        if snapshot(args.root) != snapshot(SOURCE):
            parser.error("restore the original fixture before starting a case")
        path, text, start, _ = spans(args.root, case)
        pos = position_at(text, start)
        print(f"{path}:{pos['line'] + 1}:{pos['character'] + 1} -> {case['new']}")
    elif args.command == "record":
        case = CASES[args.case]
        if not (args.root / "go.mod").is_file():
            parser.error("expected the copied workspace root")
        actual = snapshot(args.root)
        diff = list(changes(snapshot(SOURCE), actual))
        print(json.dumps({"case": args.case, "outcome": args.outcome, "observation": args.observation,
                          "changes": diff, "matchesPlannedFiles": actual == expected(case)},
                         indent=2, ensure_ascii=False))
    else:
        for case in ([CASES[args.case]] if args.case else CASES.values()):
            with tempfile.TemporaryDirectory(prefix="rs-gopls-rename-check-") as temp:
                root = shutil.copytree(SOURCE, Path(temp) / "workspace").resolve()
                spans(root, case)
                if args.command == "lsp":
                    replay(args.server, root, case)
                else:
                    for path, data in expected(case).items():
                        (root / path).write_bytes(data)
                    go_test(root)
                print(case["id"], "OK", flush=True)


if __name__ == "__main__":
    main()
