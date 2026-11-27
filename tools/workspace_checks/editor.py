"""Check single-file editor features over LSP."""

from contextlib import contextmanager
import os
import subprocess

from check_goland_completion import DocumentText
from check_goland_symbols import WORKSPACE, lsp_span
from lsp_harness import (
    WORKSPACE_ENVIRONMENT, LspClient, PausedGoList, collect_responses, position_at,
    response_detail, session, site, wait_for_response, write,
)

from ._assertions import expect_workspace_symbols


TOKENS = "textDocument/semanticTokens/full"
HIGHLIGHTS = "textDocument/documentHighlight"
HINTS = "textDocument/inlayHint"
FOLDS = "textDocument/foldingRange"

TOKEN_TYPES = [
    "namespace", "type", "struct", "interface", "typeParameter", "parameter",
    "variable", "property", "function", "method", "label",
]
TOKEN_MODIFIERS = ["declaration", "readonly", "defaultLibrary", "modification"]
DECLARATION, READONLY, DEFAULT_LIBRARY, MODIFICATION = (1, 2, 4, 8)
TEXT, READ, WRITE = (1, 2, 3)


def expect_equal(context, actual, expected):
    if actual != expected:
        raise RuntimeError(f"{context}: expected {expected!r}, got {actual!r}")


def expect_member(context, actual, expected):
    if expected not in actual:
        raise RuntimeError(f"{context}: missing {expected!r} in {actual!r}")


def editor_source(root, text):
    write(root / "go.mod", "module example.test/editor\n\ngo 1.22\n")
    return write(root / "editor.go", text)


def result_of(response):
    if "error" in response:
        raise RuntimeError(f"request {response['id']!r}: {response_detail(response)}")
    return response["result"]


def editor_results(client, reference):
    document = {"textDocument": {"uri": reference.path.as_uri()}}
    requested_range = {
        "start": {"line": 0, "character": 0},
        "end": position_at(reference.text, len(reference.text)),
    }
    requests = {
        TOKENS: client.start_request(TOKENS, **document),
        HIGHLIGHTS: client.start_request(HIGHLIGHTS, **document, position=reference.cursor()),
        HINTS: client.start_request(HINTS, **document, range=requested_range),
        FOLDS: client.start_request(FOLDS, **document),
    }
    responses = collect_responses(client, *requests.values())
    return {method: result_of(responses[request_id]) for method, request_id in requests.items()}


def point(position):
    return position["line"], position["character"]


def fold_span(fold):
    return fold["startLine"], fold["startCharacter"], fold["endLine"], fold["endCharacter"]


def token_spans(result, document):
    data = result["data"]
    if not isinstance(data, list) or not data or len(data) % 5:
        raise ValueError(f"invalid semantic token array {data!r}")
    tokens = {}
    line = start = end = 0
    for index in range(0, len(data), 5):
        row = data[index:index + 5]
        if any(type(value) is not int or value < 0 for value in row):
            raise ValueError(f"invalid semantic token {row!r}")
        delta_line, delta_start, length, token_type, modifiers = row
        line += delta_line
        start = delta_start if delta_line else start + delta_start
        if (not length or (delta_line == 0 and start < end)
                or token_type >= len(TOKEN_TYPES) or modifiers >= 1 << len(TOKEN_MODIFIERS)):
            raise ValueError(f"invalid semantic token {row!r} at {line}:{start}")
        end = start + length
        span = (line, start, line, end)
        document.offsets_for(span)
        tokens[span] = (TOKEN_TYPES[token_type], modifiers)
    return tokens


def validate_results(results, text):
    document = DocumentText(text)
    method = TOKENS
    try:
        tokens = token_spans(results[TOKENS], document)
        for method in (HIGHLIGHTS, HINTS, FOLDS):
            rows = results[method]
            if not isinstance(rows, list) or not rows:
                raise ValueError(f"expected a nonempty list, got {rows!r}")
            for row in rows:
                if method == HINTS:
                    if document.offset(point(row["position"])) >= len(text):
                        raise ValueError(f"hint is outside the requested range: {row!r}")
                    continue
                span = lsp_span(row["range"]) if method == HIGHLIGHTS else fold_span(row)
                start, end = document.offsets_for(span)
                if start == end:
                    raise ValueError(f"empty range {row!r}")
    except (KeyError, TypeError, ValueError) as exc:
        raise RuntimeError(f"{method}: {exc}") from exc
    return tokens


def expect_token(tokens, target, kind, modifiers):
    expect_equal(f"semantic token {target.text[target.start:target.end]!r}",
                 tokens.get(lsp_span(target.selection())), (kind, modifiers))


def expect_highlights(actual, *targets):
    expected = [{"range": target.selection(), "kind": kind} for target, kind in targets]
    expected.sort(key=lambda row: lsp_span(row["range"]))
    expect_equal("document highlights", actual, expected)


def expect_hint(actual, target, label, *, constant=False):
    selection = target.selection()
    if constant:
        expected = {"position": selection["end"], "label": label, "paddingLeft": True}
    else:
        expected = {"position": selection["start"], "label": label, "kind": 2, "paddingRight": True}
    expect_member("inlay hints", actual, expected)


def expect_fold(actual, opening, closing):
    expected = (*point(opening.selection()["start"]), *point(closing.selection()["end"]))
    expect_member("folding ranges", [fold_span(row) for row in actual], expected)


@contextmanager
def folding_session(executable_path, root, timeout, line_only):
    client = LspClient([executable_path], root, timeout, environment={**os.environ, **WORKSPACE_ENVIRONMENT})
    try:
        if line_only is None:
            capabilities = client.initialize()
        else:
            init = client.request(
                "initialize", processId=os.getpid(), rootUri=root.as_uri(), workspaceFolders=client.folders,
                initializationOptions={"diagnosticsTrigger": "Save"},
                capabilities={
                    "general": {"positionEncodings": ["utf-16"]},
                    "workspace": {"workspaceFolders": True},
                    "textDocument": {"foldingRange": {"lineFoldingOnly": line_only}},
                },
            )
            capabilities = init["capabilities"]
            client.notify("initialized")
        yield client, capabilities
        client.shutdown()
    except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
        log = client.stderr_tail()
        raise RuntimeError(f"{root} (lineFoldingOnly={line_only!r}): {exc}" + (f"\n{log}" if log else "")) from exc
    finally:
        client.close()


def expect_capabilities(capabilities):
    tokens = capabilities.get("semanticTokensProvider")
    if not isinstance(tokens, dict) or tokens.get("full") is not True or "range" in tokens:
        raise RuntimeError(f"expected full-only semantic tokens provider, got {tokens!r}")
    expect_equal("semantic tokens legend", tokens.get("legend"),
                 {"tokenTypes": TOKEN_TYPES, "tokenModifiers": TOKEN_MODIFIERS})
    for name in ("documentHighlightProvider", "inlayHintProvider", "foldingRangeProvider"):
        if capabilities.get(name) is not True:
            raise RuntimeError(f"expected {name}: true, got {capabilities.get(name)!r}")
    expect_equal("position encoding", capabilities.get("positionEncoding", "utf-16"), "utf-16")


def check_capabilities_and_folding_negotiation(executable_path, root, timeout):
    text = (
        "package editor\n\n"
        'import (\n\t_ "fmt"\n\t_ "os"\n)\n\n'
        "// region sample\n// first\n// second\n\n"
        "/* 🙂 block\nnote */\n"
        "var (\n\tfirst = 1\n\tsecond = 2\n)\n"
        "var (\n\tsole = []int{\n\t\t1,\n\t}\n)\n"
        "const (\n\tsoleConst = 1 +\n\t\t2\n)\n"
        "type (\n\tS struct {\n\t\tField int\n\t}\n)\n"
        "var raw = `🙂\n中文🙂`\n"
        "// endregion\n\n"
        "func work() {\n\tswitch first {\n\tcase 1:\n\t\t_ = second\n\t\t_ = sole;\n\t}\n}\n"
        "/* same line */\n"
    )
    path = editor_source(root, text)
    results = {}
    for line_only in (None, False, True):
        with folding_session(executable_path, root, timeout, line_only) as (client, capabilities):
            expect_capabilities(capabilities)
            client.open_document(path, text)
            result = client.request(FOLDS, textDocument={"uri": path.as_uri()})
            if not isinstance(result, list) or any(not isinstance(row, dict) for row in result):
                raise RuntimeError(f"invalid folding result {result!r}")
            results[line_only] = result

    expect_equal("default folding negotiation", results[None], results[False])
    columns = [
        (2, 7, 5, 1, "imports"),
        (7, 0, 33, 12, "region"),
        (7, 0, 9, 9, "comment"),
        (11, 0, 12, 7, "comment"),
        (13, 4, 16, 1, None),
        (18, 13, 20, 2, None),
        (27, 10, 29, 2, None),
        (31, 10, 32, 5, None),
        (35, 12, 41, 1, None),
        (36, 14, 40, 2, None),
        (37, 8, 39, 11, None),
        (42, 0, 42, 15, "comment"),
    ]
    if any("startCharacter" not in row or "endCharacter" not in row for row in results[False]):
        raise RuntimeError(f"missing folding columns: {results[False]!r}")
    expect_equal("column folds", [(*fold_span(row), row.get("kind")) for row in results[False]], columns)

    lines = [
        (2, 4, "imports"), (7, 33, "region"), (7, 9, "comment"), (11, 12, "comment"),
        (13, 15, None), (18, 19, None), (27, 28, None), (31, 32, None),
        (35, 40, None), (36, 39, None), (37, 39, None),
    ]
    if any("startCharacter" in row or "endCharacter" in row for row in results[True]):
        raise RuntimeError(f"unexpected columns in line-only folds: {results[True]!r}")
    expect_equal("line-only folds", [(row.get("startLine"), row.get("endLine"), row.get("kind"))
                                     for row in results[True]], lines)


def check_unsaved_edits_and_did_close_revert(executable_path, root, timeout):
    original = (
        "package editor\n\n"
        "const Size = 1 + 2\n"
        "type Record struct {\n\tCount int\n}\n\n"
        "func add(amount int) int { return amount }\n\n"
        "func run() {\n\tvar value = add(4)\nLoop:\n"
        "\tfor {\n\t\tvalue++\n\t\tbreak Loop\n\t}\n\t_ = value\n}\n"
    )
    edited = (
        "package editor\n\n"
        "const Size = 2 + 3\n"
        "type UpdatedRecord struct {\n\tTotal int\n}\n\n"
        "func double(input int) int { return input * 2 }\n\n"
        "func run() {\n\tvar value = double(4)\nOuter:\n"
        "\tfor {\n\t\tif true {\n\t\t\tvalue += 2\n\t\t}\n\t\tbreak Outer\n\t}\n\t_ = value\n}\n"
    )
    path = editor_source(root, original)

    def check_snapshot(client, text, *, record, field, function, parameter, label, write_anchor, size):
        def at(anchor):
            return site(path, anchor, text)

        reference = at("_ = [[value]]")
        results = editor_results(client, reference)
        tokens = validate_results(results, text)
        variable = at("var [[value]] =")
        modified = at(write_anchor)
        label_definition = at(f"[[{label}]]:")
        label_use = at(f"break [[{label}]]")
        expect_token(tokens, at("const [[Size]]"), "variable", DECLARATION | READONLY)
        expect_token(tokens, at(f"type [[{record}]]"), "struct", DECLARATION)
        expect_token(tokens, at(f"[[{field}]] int"), "property", DECLARATION)
        expect_token(tokens, at(f"func [[{function}]]("), "function", DECLARATION)
        expect_token(tokens, label_definition, "label", DECLARATION)
        expect_token(tokens, label_use, "label", 0)
        expect_token(tokens, modified, "variable", MODIFICATION)
        expect_highlights(results[HIGHLIGHTS], (variable, WRITE), (modified, WRITE), (reference, READ))
        label_highlights = client.request(HIGHLIGHTS, textDocument={"uri": path.as_uri()}, position=label_use.cursor())
        expect_highlights(label_highlights, (label_definition, TEXT), (label_use, TEXT))
        expect_hint(results[HINTS], at("const [[Size]]"), f"= {size}", constant=True)
        expect_hint(results[HINTS], at(f"{function}([[4]])"), f"{parameter}:")
        expect_fold(results[FOLDS], at("func run() [[{]]"), at("\t_ = value\n[[}]]"))
        return results

    with session(executable_path, root, timeout) as client:
        client.open_document(path, original)
        before = check_snapshot(
            client, original, record="Record", field="Count", function="add", parameter="amount",
            label="Loop", write_anchor="[[value]]++", size=3,
        )
        client.change_document(path, 2, edited)
        after = check_snapshot(
            client, edited, record="UpdatedRecord", field="Total", function="double", parameter="input",
            label="Outer", write_anchor="[[value]] += 2", size=5,
        )
        expect_fold(after[FOLDS], site(path, "if true [[{]]", edited),
                    site(path, "value += 2\n\t\t[[}]]", edited))
        for method, result in after.items():
            if result == before[method]:
                raise RuntimeError(f"{method} returned the same result after the unsaved edit")
        expect_equal("unsaved edit on disk", path.read_bytes(), original.encode("utf-8"))

        client.close_document(path)
        restored = editor_results(client, site(path, "_ = [[value]]", original))
        for method, result in before.items():
            expect_equal(f"{method} after didClose", restored[method], result)


def check_utf16_crlf_non_bmp_and_malformed_syntax(executable_path, root, timeout):
    text = (
        "package editor\r\n"
        'const Width = len("中文🙂")\r\n'
        "var Current int\r\n"
        "func complete() {\r\n"
        '\t/* 中文🙂 */ _ = Current; _ = len("ok"); _ = []int{\r\n'
        "\t\t1,\r\n\t}\r\n}\r\n"
        "func unfinished() {\r\n\t_ = 1 +\r\n"
    )
    path = editor_source(root, text)

    def at(anchor):
        return site(path, anchor, text)

    reference = at("_ = [[Current]];")
    with session(executable_path, root, timeout) as client:
        client.open_document(path, text)
        results = editor_results(client, reference)
    tokens = validate_results(results, text)
    expect_token(tokens, reference, "variable", 0)
    expect_token(tokens, at("const [[Width]]"), "variable", DECLARATION | READONLY)
    expect_token(tokens, at('[[len]]("中文🙂")'), "function", DEFAULT_LIBRARY)
    expect_highlights(results[HIGHLIGHTS], (at("var [[Current]] int"), WRITE), (reference, READ))
    expect_hint(results[HINTS], at("const [[Width]]"), "= 10", constant=True)
    expect_hint(results[HINTS], at('len([["ok"]])'), "v:")
    expect_fold(results[FOLDS], at("func complete() [[{]]"), at("\r\n\t}\r\n[[}]]"))
    expect_fold(results[FOLDS], at("[]int[[{]]"), at("1,\r\n\t[[}]]"))
    unfinished = point(at("func unfinished() [[{]]").selection()["start"])
    if any(fold_span(row)[:2] == unfinished for row in results[FOLDS]):
        raise RuntimeError(f"unexpected fold for the unclosed function: {results[FOLDS]!r}")


def expect_no_extra_responses(client):
    try:
        response = client.get_next_response(timeout=0.2)
    except TimeoutError:
        return
    raise RuntimeError(f"unexpected or duplicate response: {response!r}")


def check_zero_blocking_under_paused_go_list(executable_path, root, timeout):
    gate = PausedGoList(root / "control", timeout)
    project = root / "project"
    text = "package editor\nconst Size = 1 + 2\nfunc run() {\n\t_ = Size\n}\n"
    path = editor_source(project, text)
    witness = write(project / "witness.go", "package editor\nvar EditorGraphWitness int\n")
    reference = site(path, "_ = [[Size]]", text)
    declaration = site(path, "const [[Size]]", text)

    with session(executable_path, project, timeout, **gate.environment) as client:
        client.open_document(path, text)
        with gate.pause():
            graph_id = client.start_request(WORKSPACE, query="EditorGraphWitness")
            gate.wait_for_snapshot()
            paused = editor_results(client, reference)
            expect_no_extra_responses(client)
            if (gate.control / "done").exists():
                raise RuntimeError("the Go list relay finished before the gate was released")
            tokens = validate_results(paused, text)
            expect_token(tokens, reference, "variable", READONLY)
            expect_highlights(paused[HIGHLIGHTS], (declaration, WRITE), (reference, READ))
            expect_hint(paused[HINTS], declaration, "= 3", constant=True)
            expect_fold(paused[FOLDS], site(path, "func run() [[{]]", text),
                        site(path, "_ = Size\n[[}]]", text))

        graph = result_of(wait_for_response(client, graph_id))
        expect_workspace_symbols(graph, site(witness, "var [[EditorGraphWitness]]"), container="editor")
        ready = editor_results(client, reference)
        for method, result in paused.items():
            expect_equal(f"{method} with ready package graph", ready[method], result)
        expect_no_extra_responses(client)


CASES = (
    ("editor_capabilities_and_folding_negotiation", check_capabilities_and_folding_negotiation),
    ("editor_unsaved_edits_and_did_close_revert", check_unsaved_edits_and_did_close_revert),
    ("editor_utf16_crlf_non_bmp_and_malformed_syntax", check_utf16_crlf_non_bmp_and_malformed_syntax),
    ("editor_zero_blocking_under_paused_go_list", check_zero_blocking_under_paused_go_list),
)
