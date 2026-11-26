#!/usr/bin/env python3
"""Check formatting, organize imports, and quick fixes by applying their LSP edits."""

from functools import cache
from pathlib import Path
import subprocess
import sys

from bench_common import (
    START, Checked, Fixture, apply_text_edits, check_main, go_output, line_ending,
    overlay_prefix, range_offsets, relative_uri, shift_range,
)
from check_goland_definitions import query_site
from check_goland_diagnostics import full_diagnostics, overlaps, pull_diagnostics


FORMATTING = "textDocument/formatting"
ORGANIZE = "source.organizeImports"
QUICKFIX = "quickfix"
CGO_PREAMBLE = '/*\n#include <stdlib.h>\n*/\nimport "C"'
MESSY_IMPORTS = (
    'import (\n\t"fmt"\n\t"strings"\n\t_ "time/tzdata"\n\n'
    '\t// The shared prefix keeps labels consistent across commands.\n\t"example.test/dependency"\n\n'
    '\t"example.test/actions/helper" // Matches the labels used by built-in commands.\n)'
)
GOLAND_IMPORTS = (
    'import (\n\t"strings"\n\n'
    '\t"example.test/actions/helper" // Matches the labels used by built-in commands.\n'
    '\t"fmt"\n\t_ "time/tzdata"\n\n'
    '\t// The shared prefix keeps labels consistent across commands.\n\tdependency "example.test/dependency"\n)'
)
QUICK_FIXES = {
    "unused-import": ('import "strings"\n\n', ""),
    "missing-stdlib-import": ('import "strings"', 'import (\n\t"fmt"\n\t"strings"\n)'),
    "missing-local-import": ('import "strings"', 'import (\n\t"strings"\n\n\t"example.test/actions/helper"\n)'),
    "unused-variable": ("unused := 42", "_ = 42"),
    "no-new-variables": ("value := value + 1", "value = value + 1"),
}


def with_line_endings(text, original):
    return text.replace("\r\n", "\n").replace("\n", line_ending(original))


def replace_once(text, old, new):
    old, new = (with_line_endings(value, text) for value in (old, new))
    if text.count(old) != 1:
        raise ValueError("fixture does not have the expected edit anchor")
    return text.replace(old, new, 1)


def organize_expected(case_id, text, *, goland=False):
    if case_id == "organize-cgo-imports":
        imports = 'import (\n\t"unsafe"\n)' if goland else 'import "unsafe"'
        return replace_once(text, 'import (\n\t"fmt"\n\t"unsafe"\n)', imports)
    ending = line_ending(text)
    begin = text.index("import (")
    end = text.index(ending + ")", begin) + len(ending) + 1
    imports = GOLAND_IMPORTS if goland else MESSY_IMPORTS
    return text[:begin] + with_line_endings(imports, text) + text[end:]


def action_edits(action, target, root):
    if "disabled" in action or not isinstance(action.get("title"), str):
        raise ValueError("expected an enabled code action with a title")
    edit = action.get("edit")
    if not isinstance(edit, dict) or ("changes" in edit) == ("documentChanges" in edit):
        raise ValueError("expected an inline WorkspaceEdit with one edit representation")
    if "changes" in edit:
        entries = list(edit["changes"].items())
    else:
        if any("textDocument" not in change for change in edit["documentChanges"]):
            raise ValueError("code action returned a resource operation")
        entries = [(change["textDocument"]["uri"], change["edits"]) for change in edit["documentChanges"]]
    if len(entries) != 1 or relative_uri(entries[0][0], root) != target:
        raise ValueError("code action must edit only the requested document")
    if not isinstance(entries[0][1], list) or not entries[0][1]:
        raise ValueError("code action returned no edits")
    return entries[0][1]


class Actions(Fixture):
    track = "actions"
    default_method = None
    providers = ("documentFormattingProvider", "codeActionProvider", "diagnosticProvider")
    capture_fields = ("before", "after")
    goland_timing = "Go SDK gofmt subprocess or smart-mode import optimization on a PSI copy"

    def __init__(self, args):
        super().__init__(args)
        self.selections = {}
        for case_id, case in self.cases.items():
            if case["request"] not in (FORMATTING, ORGANIZE, QUICKFIX):
                raise ValueError(f"invalid action request for {case_id}")
            if case["request"] == QUICKFIX:
                relative = case["path"]
                self.selections[case_id] = query_site(
                    self.path(relative), self.sources[relative], case, case["token"],
                ).selection()
        sdk = go_output("env", "GOROOT", cwd=self.root, environment=self.environment, timeout=self.timeout)
        self.gofmt = Path(sdk) / "bin" / ("gofmt.exe" if sys.platform == "win32" else "gofmt")

    @cache
    def gofmt_output(self, text):
        output = subprocess.run(
            [str(self.gofmt)], cwd=self.root, env=self.environment, input=text.encode("utf-8"),
            capture_output=True, check=True, timeout=self.timeout,
        ).stdout.decode("utf-8")
        if text.strip() and not output.strip():
            raise ValueError("gofmt returned empty text for a non-empty document")
        return with_line_endings(output, text)

    def request(self, client, case, text, shift):
        path = self.path(case["path"])
        document = {"uri": path.as_uri()}
        method = case["request"]
        if method == FORMATTING:
            result = client.request(FORMATTING, textDocument=document, options={"insertSpaces": False, "tabSize": 4})
            return result, []
        diagnostics = []
        selection = {"start": START, "end": START}
        if method == QUICKFIX:
            selection = shift_range(self.selections[case["id"]], shift)
            selected = range_offsets(text, selection)
            diagnostics = [
                item for item in full_diagnostics(pull_diagnostics(client, path))
                if item.get("code") == case["diagnosticCode"] and overlaps(range_offsets(text, item["range"]), selected)
            ]
            if len(diagnostics) != 1:
                raise ValueError(f"expected one {case['diagnosticCode']} diagnostic at {selection!r}")
        result = client.request(
            "textDocument/codeAction", textDocument=document, range=selection,
            context={"diagnostics": diagnostics, "only": [method]},
        )
        return result, diagnostics

    def validate(self, case, raw, text, shift):
        case_id, method, relative = case["id"], case["request"], case["path"]
        response, diagnostics = raw
        original = self.sources[relative]
        capture = self.baseline.get(case_id)
        captured = None
        if capture is not None:
            if with_line_endings(capture["before"], original) != original:
                raise ValueError("GoLand capture input differs from the fixture source")
            captured = with_line_endings(capture["after"], original)

        if method == FORMATTING:
            expected = self.gofmt_output(text)
            original_expected = self.gofmt_output(original)
            edits = [] if response is None else response
            if not isinstance(edits, list) or (edits and expected == text):
                raise ValueError("formatting must return a text edit array and leave clean files untouched")
        else:
            if method == QUICKFIX and case_id in QUICK_FIXES:
                original_expected = replace_once(original, *QUICK_FIXES[case_id])
            elif method == ORGANIZE and case_id in ("organize-imports", "organize-cgo-imports"):
                original_expected = organize_expected(case_id, original)
            elif captured is not None:
                original_expected = captured
            else:
                raise ValueError("action query requires a GoLand result fixture")
            expected = (overlay_prefix(text) if shift else "") + original_expected
            actions = [] if response is None else response
            if not isinstance(actions, list) or len(actions) != int(expected != text):
                raise ValueError(f"expected {int(expected != text)} {method} action(s)")
            edits = []
            if actions:
                action = actions[0]
                if not isinstance(action, dict) or action.get("kind") != method:
                    raise ValueError(f"expected a {method} code action")
                if method == QUICKFIX and action.get("diagnostics") != diagnostics:
                    raise ValueError("quick fix is not associated with the selected diagnostic")
                edits = action_edits(action, relative, self.root)

        if relative == "imports_cgo.go" and method == ORGANIZE:
            preamble = with_line_endings(CGO_PREAMBLE, text)
            start = text.index(preamble)
            end = start + len(preamble)
            for edit in edits:
                edit_start, edit_end = range_offsets(text, edit["range"])
                if overlaps((edit_start, edit_end), (start, end)):
                    raise ValueError("organize imports edited the cgo preamble or import")
        actual = apply_text_edits(text, edits)
        if actual != expected:
            raise ValueError(f"edited text expected {expected!r}, received {actual!r}")
        if captured is None or captured == original_expected:
            return Checked(original_expected)
        if method != ORGANIZE or captured != organize_expected(case_id, original, goland=True):
            raise ValueError("GoLand output differs from the expected action text")
        reason = ("GoLand preserves a one-entry import group" if case_id == "organize-cgo-imports" else
                  "GoLand preserves different import groups and the dependency alias")
        return Checked(original_expected, ({"reason": reason, "goland": captured, "rs_gopls": original_expected},))


def main():
    return check_main(Actions, __doc__)


if __name__ == "__main__":
    raise SystemExit(main())
