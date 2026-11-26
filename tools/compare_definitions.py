#!/usr/bin/env python3
"""Check and time cross-package LSP definition on a small Go workspace."""

import argparse
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import sys
import tempfile
import time

from lsp_harness import LspClient, executable, path_from_uri, position_at


MODULE = "example.com/definitionbench"
TARGET = "package dependency\n\nfunc Left() {}\n\nfunc Right() {}\n"


def caller(symbol, stdlib=False):
    import_path = "net/http" if stdlib else f"{MODULE}/dependency"
    statement = f"_ = helper.{symbol}" if stdlib else f"helper.{symbol}()"
    return (
        f'package caller\n\nimport helper "{import_path}"\n\n'
        f"func run() {{ /* \U0001f642 */ {statement} }}\n"
    )


def point_in_caller(text, symbol):
    offset = text.index(f"helper.{symbol}") + len("helper.")
    return position_at(text, offset + 1)


def verify_location(result, target_path, target_source, symbol):
    locations = result if isinstance(result, list) else [result] if result else []
    if len(locations) != 1:
        raise RuntimeError(f"expected one definition for {symbol}, received {result!r}")
    location = locations[0]
    uri = location.get("targetUri", location.get("uri"))
    span = location.get("targetSelectionRange", location.get("range"))
    start = target_source.index(f"func {symbol}(") + len("func ")
    expected = {"start": position_at(target_source, start), "end": position_at(target_source, start + len(symbol))}
    if path_from_uri(uri) != target_path or span != expected:
        raise RuntimeError(f"{symbol}: expected {target_path} at {expected!r}, received {location!r}")


def definition(client, caller_path, caller_source, target_path, target_source, symbol):
    result = client.request(
        "textDocument/definition",
        textDocument={"uri": caller_path.as_uri()},
        position=point_in_caller(caller_source, symbol),
    )
    end = time.perf_counter_ns()
    verify_location(result, target_path, target_source, symbol)
    return end


def measure(command, root, iterations, timeout, stdlib_target):
    root.mkdir()
    caller_path = root / "caller.go"
    if stdlib_target:
        target_path = stdlib_target
        target_source = target_path.read_text(encoding="utf-8")
        symbols = ("ListenAndServe", "NewServeMux")
    else:
        target_path = (root / "dependency" / "dependency.go").resolve()
        target_path.parent.mkdir()
        target_source = TARGET
        target_path.write_text(target_source, encoding="utf-8", newline="\n")
        symbols = ("Left", "Right")

    original = caller(symbols[0], stdlib=bool(stdlib_target))
    (root / "go.mod").write_text(f"module {MODULE}\n\ngo 1.22\n", encoding="utf-8", newline="\n")
    caller_path.write_text(original, encoding="utf-8", newline="\n")

    started = time.perf_counter_ns()
    client = LspClient(command, root, timeout)
    try:
        client.initialize()
        client.open_document(caller_path, original)
        first = (definition(client, caller_path, original, target_path, target_source, symbols[0]) - started) / 1e6

        samples = []
        for version in range(2, iterations + 2):
            symbol = symbols[(version - 1) % len(symbols)]
            edited = caller(symbol, stdlib=bool(stdlib_target))
            started = time.perf_counter_ns()
            client.change_document(caller_path, version, edited)
            end = definition(client, caller_path, edited, target_path, target_source, symbol)
            samples.append((end - started) / 1e6)

        if not stdlib_target:
            # The dependency overlay moves the declaration and closing it restores the disk range.
            target_overlay = "// unsaved dependency change\n" + target_source
            client.open_document(target_path, target_source)
            client.change_document(target_path, 2, target_overlay)
            definition(client, caller_path, edited, target_path, target_overlay, symbol)
            client.close_document(target_path)
            definition(client, caller_path, edited, target_path, target_source, symbol)
        client.close_document(caller_path)
        client.shutdown()
    except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
        log = client.stderr_tail()
        raise RuntimeError(f"{command[0]}: {exc}" + (f"\n{log}" if log else "")) from exc
    finally:
        client.close()

    return first, statistics.median(samples), sorted(samples)[math.ceil(len(samples) * 0.95) - 1]


def net_http_source(root, timeout):
    result = subprocess.run(
        ["go", "env", "GOROOT"],
        cwd=root,
        env={**os.environ, "GOWORK": "off", "GOPROXY": "off"},
        capture_output=True,
        encoding="utf-8",
        timeout=timeout,
        check=True,
    )
    source = (Path(result.stdout.strip()) / "src" / "net" / "http" / "server.go").resolve()
    text = source.read_text(encoding="utf-8")
    if any(f"\nfunc {name}(" not in text for name in ("ListenAndServe", "NewServeMux")):
        raise RuntimeError(f"expected both package-level declarations in {source}")
    return source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=executable, help="path to the candidate LSP executable")
    parser.add_argument("--baseline", nargs="?", const="gopls", type=executable, help="compare against gopls (optionally give its path)")
    parser.add_argument("--iterations", type=int, default=30, help="timed unsaved caller edits per server")
    parser.add_argument("--stdlib", action="store_true", help="use a synthetic caller importing the SDK's net/http")
    parser.add_argument("--timeout", type=float, default=60.0, help="seconds allowed for each LSP request")
    args = parser.parse_args()
    if args.iterations < 1 or not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("iterations and a finite timeout must be positive")
    commands = [("candidate", args.candidate)]
    if args.baseline:
        commands.append(("baseline", args.baseline))

    try:
        with tempfile.TemporaryDirectory(prefix="rs-gopls-definition-") as directory:
            root = Path(directory).resolve()
            stdlib_target = net_http_source(root, args.timeout) if args.stdlib else None
            workload = "Synthetic caller importing the SDK's net/http." if args.stdlib else "Synthetic fixture only."
            print(f"{workload} Startup is a single sample with shared system and Go caches.", flush=True)
            print(f"{'server':12} {'start to definition (ms)':>24} {'post-edit P50 (ms)':>20} {'post-edit P95 (ms)':>20}", flush=True)
            for name, command in commands:
                first, p50, p95 = measure([command], root / name, args.iterations, args.timeout, stdlib_target)
                print(f"{name:12} {first:24.2f} {p50:20.2f} {p95:20.2f}", flush=True)
    except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
        print(f"Verification failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
