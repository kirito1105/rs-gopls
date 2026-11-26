#!/usr/bin/env python3
"""Check and time cross-package LSP definition on a small Go workspace."""

import argparse
import json
import math
import os
from pathlib import Path
from queue import Empty, Queue
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from urllib.parse import urlsplit
from urllib.request import url2pathname


MODULE = "example.com/definitionbench"
TARGET = "package dependency\n\nfunc Left() {}\n\nfunc Right() {}\n"


def caller(symbol, stdlib=False):
    import_path = "net/http" if stdlib else f"{MODULE}/dependency"
    statement = f"_ = helper.{symbol}" if stdlib else f"helper.{symbol}()"
    return (
        f'package caller\n\nimport helper "{import_path}"\n\n'
        f"func run() {{ /* \U0001f642 */ {statement} }}\n"
    )


def position_at(text, offset):
    return {
        "line": text.count("\n", 0, offset),
        "character": len(text[text.rfind("\n", 0, offset) + 1 : offset].encode("utf-16-le")) // 2,
    }


def point_in_caller(text, symbol):
    offset = text.index(f"helper.{symbol}") + len("helper.")
    return position_at(text, offset + 1)


class LspClient:
    def __init__(self, command, root, timeout, *, environment=None):
        self.timeout = timeout
        self.next_id = 0
        self.inbox = Queue()
        self.folders = [{"uri": root.as_uri(), "name": root.name}]
        self.stderr = tempfile.TemporaryFile(mode="w+b")
        try:
            self.process = subprocess.Popen(
                command,
                cwd=root,
                env=environment if environment is not None else {**os.environ, "GOWORK": "off", "GOPROXY": "off"},
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=self.stderr,
            )
        except OSError:
            self.stderr.close()
            raise
        self.reader = threading.Thread(target=self.read_messages, daemon=True)
        self.reader.start()

    def read_messages(self):
        output = self.process.stdout
        try:
            while True:
                headers = {}
                while (line := output.readline()).strip():
                    key, value = line.split(b":", 1)
                    headers[key.strip().lower()] = value.strip()
                if not line:
                    raise EOFError("server closed stdout")
                size = int(headers[b"content-length"])
                body = output.read(size)
                if len(body) != size:
                    raise EOFError("server truncated an LSP message")
                self.inbox.put(json.loads(body))
        except (ValueError, KeyError, OSError, EOFError) as exc:
            self.inbox.put(exc)

    def send(self, **message):
        self.send_batch(message)

    def send_batch(self, *messages):
        frames = bytearray()
        for message in messages:
            body = json.dumps({"jsonrpc": "2.0", **message}, ensure_ascii=False).encode("utf-8")
            frames.extend(f"Content-Length: {len(body)}\r\n\r\n".encode("ascii"))
            frames.extend(body)
        self.process.stdin.write(frames)
        self.process.stdin.flush()

    def notify(self, method, **params):
        self.send(method=method, params=params)

    def reply(self, message):
        request_id = message["id"]
        method = message["method"]
        if method == "workspace/workspaceFolders":
            self.send(id=request_id, result=self.folders)
        elif method == "workspace/configuration":
            self.send(id=request_id, result=[None] * len(message["params"]["items"]))
        elif method in {"window/workDoneProgress/create", "client/registerCapability", "client/unregisterCapability"}:
            self.send(id=request_id, result=None)
        else:
            self.send(id=request_id, error={"code": -32601, "message": f"Unsupported client method: {method}"})

    def request(self, method, **params):
        self.next_id += 1
        request_id = self.next_id
        self.send(id=request_id, method=method, params=params)
        deadline = time.monotonic() + self.timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RuntimeError(f"{method} timed out after {self.timeout:g}s")
            try:
                message = self.inbox.get(timeout=remaining)
            except Empty as exc:
                raise RuntimeError(f"{method} timed out after {self.timeout:g}s") from exc
            if isinstance(message, Exception):
                raise RuntimeError(f"{method}: {message}") from message
            if "method" in message:
                if "id" in message:
                    self.reply(message)
                continue
            if message.get("id") == request_id:
                if "error" in message:
                    raise RuntimeError(f"{method} failed: {message['error']}")
                return message["result"]

    def initialize(self, *, watch_files=False, hierarchical_document_symbols=None):
        workspace = {"workspaceFolders": True}
        if watch_files:
            workspace["didChangeWatchedFiles"] = {"dynamicRegistration": True}
        capabilities = {
            "general": {"positionEncodings": ["utf-16"]},
            "workspace": workspace,
        }
        if hierarchical_document_symbols is not None:
            capabilities["textDocument"] = {
                "documentSymbol": {"hierarchicalDocumentSymbolSupport": hierarchical_document_symbols},
            }
        init = self.request(
            "initialize",
            processId=os.getpid(),
            rootUri=self.folders[0]["uri"],
            workspaceFolders=self.folders,
            initializationOptions={"diagnosticsTrigger": "Save"},
            capabilities=capabilities,
        )
        capabilities = init["capabilities"]
        if capabilities.get("definitionProvider") in (None, False):
            raise RuntimeError("server did not advertise definitionProvider")
        if capabilities.get("positionEncoding", "utf-16").lower() != "utf-16":
            raise RuntimeError("server did not negotiate UTF-16 positions")
        self.notify("initialized")
        return capabilities

    def open_document(self, path, text):
        self.notify(
            "textDocument/didOpen",
            textDocument={"uri": path.as_uri(), "languageId": "go", "version": 1, "text": text},
        )

    def change_document(self, path, version, text):
        self.notify(
            "textDocument/didChange",
            textDocument={"uri": path.as_uri(), "version": version},
            contentChanges=[{"text": text}],
        )

    def change_document_range(self, path, version, start, end, text):
        self.notify(
            "textDocument/didChange",
            textDocument={"uri": path.as_uri(), "version": version},
            contentChanges=[{"range": {"start": start, "end": end}, "text": text}],
        )

    def close_document(self, path):
        self.notify("textDocument/didClose", textDocument={"uri": path.as_uri()})

    def shutdown(self):
        self.request("shutdown")
        self.notify("exit")
        self.process.wait(timeout=min(self.timeout, 5))

    def stderr_tail(self):
        size = self.stderr.seek(0, os.SEEK_END)
        self.stderr.seek(max(0, size - 4096))
        return self.stderr.read().decode("utf-8", errors="replace").strip()

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)
        self.process.stdin.close()
        self.reader.join(timeout=2)
        self.process.stdout.close()
        self.stderr.close()


def path_from_uri(uri):
    components = urlsplit(uri)
    if components.scheme != "file":
        raise RuntimeError(f"expected a file URI, received {uri!r}")
    path = ("//" + components.netloc if components.netloc and components.netloc != "localhost" else "") + components.path
    return Path(url2pathname(path)).resolve()


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


def executable(value):
    resolved = shutil.which(value)
    if not resolved:
        raise argparse.ArgumentTypeError(f"executable not found: {value}")
    return str(Path(resolved).resolve())


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
