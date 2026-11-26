#!/usr/bin/env python3
"""Check pull diagnostics, GoLand highlights, and unsaved recovery over stdio LSP."""

import re

from bench_common import Checked, Fixture, check_main, line_ending, range_offsets, shift_range
from lsp_harness import site


DIAGNOSTIC = "textDocument/diagnostic"
ANCHORS = {
    "clean/clean.go": (),
    "clean/clean_test.go": (),
    "broken_syntax/missing_paren.go": (("syntax.error", 'buffer[["]]'),),
    "broken_syntax/missing_expression.go": (("syntax.error", '+ [[)]]'),),
    "semantic_errors/imports.go": (("import.unused", 'import [["strings"]]'),),
    "semantic_errors/self_import/self.go": (
        ("import.self", 'import _ [["example.test/diagnostics/semantic_errors/self_import"]]'),
    ),
    "semantic_errors/invalid_import/invalid.go": (
        ("import.unresolved", 'import _ [["example.test/diagnostics/nonexistent"]]'),
    ),
    "semantic_errors/names.go": (
        ("name.unresolved", "return [[missingValue]]"),
        ("type.unresolved", "UnresolvedType [[MissingType]]"),
        ("field.unresolved", "record.[[MissingField]]"),
        ("method.unresolved", "record.[[MissingMethod]]"),
    ),
    "semantic_errors/declarations.go": (
        ("name.redeclared", "var [[Repeated]]"),
        ("var.unused", "[[unused]] := 1"),
        ("shortvar.no_new_vars", "value [[:=]] value"),
        ("const.reassigned", "[[Fixed]] = 2"),
    ),
    "semantic_errors/calls.go": (
        ("call.arity", "return Add[[(1)]]"),
        ("return.arity", "[[return 1]]"),
        ("call.non_function", "[[value]]()"),
    ),
    "semantic_errors/types.go": (
        ("type.mismatch", '= [["text"]]'),
        ("type.interface_not_implemented", "= [[Item{}]]"),
        ("field.unknown", "Item{[[Missing]]:"),
        ("switch.duplicate_case", "case [[1]]:\n\t\treturn 2"),
        ("name.blank_as_value", "BlankValue = [[_]]"),
    ),
}
LSP_CODES = {
    "import.unresolved": "import.invalid",
    "field.unresolved": "member.unresolved",
    "method.unresolved": "member.unresolved",
    "const.reassigned": "const.reassign",
    "field.unknown": "literal.unknown_field",
    "name.blank_as_value": "expr.blank_as_value",
}
GOLAND_MESSAGES = {
    "syntax.error": r"expected",
    "import.unused": r"^Unused import",
    "import.self": r"^Self import is not allowed",
    "import.unresolved": r"^Cannot resolve symbol",
    "name.unresolved": r"^Unresolved reference",
    "type.unresolved": r"^Unresolved type",
    "field.unresolved": r"^Unresolved reference",
    "method.unresolved": r"^Unresolved reference",
    "name.redeclared": r"redeclared in this package",
    "var.unused": r"^Unused variable",
    "shortvar.no_new_vars": r"^No new variables",
    "const.reassigned": r"^Cannot assign to",
    "call.arity": r"arguments in call",
    "return.arity": r"arguments to return",
    "call.non_function": r"^Cannot call the nonfunction",
    "type.mismatch": r"cannot be represented by the type",
    "type.interface_not_implemented": r"does not implement",
    "field.unknown": r"^Unknown field",
    "switch.duplicate_case": r"^Duplicate case",
    "name.blank_as_value": r"^Cannot use '_' as a value",
}
GOLAND_EXTRA = {
    "self-import": ("package.cycle", "[[package self_import]]", "GoLand reports the import cycle on the package clause"),
    "declarations": ("name.redeclared", "const [[Repeated]]", "GoLand also marks the first declaration"),
    "types": ("switch.duplicate_case", "case [[1]]:\n\t\treturn 1", "GoLand also marks the first switch case"),
}
REPAIRS = {"missing-paren": ('len("buffer"', 'len("buffer")'), "missing-expression": ("+ )", "+ 1")}


def pull_diagnostics(client, path, previous=None):
    params = {"textDocument": {"uri": path.as_uri()}}
    if previous is not None:
        params["previousResultId"] = previous
    return client.request(DIAGNOSTIC, **params)


def full_diagnostics(report):
    if not isinstance(report, dict) or report.get("kind") != "full" or not isinstance(report.get("items"), list):
        raise ValueError("expected a full diagnostic report with an items array")
    return report["items"]


def overlaps(left, right):
    if left[0] == right[0]:
        return True
    return left[0] < right[1] and right[0] < left[1]


def goland_category(item, category):
    code = item.get("category", item.get("code"))
    if code is not None:
        return code in (category, LSP_CODES.get(category))
    if category == "package.cycle":
        return item.get("inspection") == "GoCyclicImports"
    return bool(re.search(GOLAND_MESSAGES[category], item.get("description", ""), flags=re.IGNORECASE))


def goland_differences(case_id, path, text, expected, diagnostics):
    remaining = list(diagnostics)
    differences = []

    def take(category, span, *, allow_overlap=False):
        wanted = range_offsets(text, span)
        for index, item in enumerate(remaining):
            found = range_offsets(text, item["range"])
            if goland_category(item, category) and (found == wanted or (allow_overlap and overlaps(found, wanted))):
                return remaining.pop(index)
        return None

    for category, span in expected:
        reference_span, reason = span, "GoLand highlights a different span for the same diagnostic"
        if case_id == "declarations" and category == "shortvar.no_new_vars":
            reference_span = site(path, "[[value]] := value", text).selection()
            reason = "GoLand highlights the short declaration variable and LSP highlights the operator"
        elif case_id == "missing-paren":
            reference_span = {"start": span["end"], "end": span["end"]}
            reason = "GoLand uses a missing-token position and LSP highlights the preceding character"
        elif case_id == "missing-expression":
            reference_span = site(path, "+[[ ]])", text).selection()
            reason = "GoLand highlights the space before the unexpected token and LSP highlights the token"
        match = take(category, span) or take(category, reference_span, allow_overlap=True)
        if match is None:
            raise ValueError(f"GoLand capture lacks {category} at {reference_span!r}")
        if match["range"] != span:
            differences.append({"category": category, "reason": reason, "goland": match["range"], "rs_gopls": span})

    if case_id in GOLAND_EXTRA:
        category, anchor, reason = GOLAND_EXTRA[case_id]
        extra = take(category, site(path, anchor.replace("\n", line_ending(text)), text).selection())
        if extra is not None:
            differences.append({"category": category, "reason": reason, "goland": extra["range"]})
    if remaining:
        raise ValueError(f"GoLand capture has {len(remaining)} unexpected diagnostic(s)")
    return differences


class Diagnostics(Fixture):
    track = "diagnostics"
    default_method = DIAGNOSTIC
    providers = ("diagnosticProvider",)
    capture_fields = ("diagnostics",)
    goland_timing = "smart-mode syntax and inspection passes through highlight collection"

    def __init__(self, args):
        super().__init__(args)
        expected_paths = {source.relative_to(self.root).as_posix() for source in self.root.rglob("*.go")}
        if set(self.sources) != expected_paths or len(self.cases) != len(self.sources):
            raise ValueError("diagnostic fixture must cover each Go source exactly once")

    def request(self, client, case, text, shift):
        return pull_diagnostics(client, self.path(case["path"]))

    def validate(self, case, report, text, shift):
        relative, case_id = case["path"], case["id"]
        path, original = self.path(relative), self.sources[relative]
        anchors = ANCHORS[relative]
        if sorted(case["expectedCategories"]) != sorted(category for category, _ in anchors):
            raise ValueError("query categories differ from the fixture's expected source errors")
        items = list(full_diagnostics(report))
        differences, normalized = [], []
        expected = []
        for category, anchor in anchors:
            span = site(path, anchor.replace("\n", line_ending(original)), original).selection()
            code = LSP_CODES.get(category, category)
            wanted = shift_range(span, shift)
            for index, item in enumerate(items):
                if item.get("code") == code and item["range"] == wanted:
                    range_offsets(text, item["range"])
                    items.pop(index)
                    break
            else:
                actual = [{"code": item.get("code"), "range": item.get("range")} for item in items]
                raise ValueError(f"missing {code} at {wanted!r}. Remaining diagnostics {actual!r}")
            expected.append((category, span))
            normalized.append({"category": category, "code": code, "range": span})
        for item in items:
            if item.get("code") != "package.load" or case_id not in (*REPAIRS, "self-import", "invalid-import"):
                raise ValueError(f"unexpected diagnostic {item.get('code')!r} at {item.get('range')!r}")
            range_offsets(text, item["range"])
            differences.append({"code": "package.load", "reason": "LSP also reports Go package loading errors",
                                "rs_gopls": item["range"]})
        if case_id in self.baseline:
            differences.extend(goland_differences(
                case_id, path, original, expected, self.baseline[case_id]["diagnostics"],
            ))
        return Checked(normalized, tuple(differences))


def check_recovery(fixture, client):
    unchanged, recovered = [], []
    for case_id, case in fixture.cases.items():
        relative = case["path"]
        path, text = fixture.path(relative), fixture.sources[relative]
        before = pull_diagnostics(client, path)
        items = full_diagnostics(before)
        previous = before.get("resultId")
        if not isinstance(previous, str) or not previous:
            raise ValueError(f"{case_id}: full diagnostic result has no resultId")
        repeat = pull_diagnostics(client, path, previous)
        if repeat.get("kind") != "unchanged" or repeat.get("resultId") != previous or "items" in repeat:
            raise ValueError(f"{case_id}: previousResultId did not return unchanged")
        unchanged.append(case_id)
        if case_id in REPAIRS:
            old, new = REPAIRS[case_id]
            if text.count(old) != 1:
                raise ValueError(f"{case_id}: expected one unsaved recovery anchor")
            client.change_document(path, 2, text.replace(old, new, 1))
            repaired = pull_diagnostics(client, path, previous)
            if full_diagnostics(repaired) or repaired.get("resultId") in (None, previous):
                raise ValueError(f"{case_id}: unsaved repair did not clear the diagnostics")
            client.change_document(path, 3, text)
            restored = pull_diagnostics(client, path, repaired["resultId"])
            if full_diagnostics(restored) != items or restored.get("resultId") == repaired["resultId"]:
                raise ValueError(f"{case_id}: reverted buffer did not restore the diagnostics")
            if path.read_bytes().decode("utf-8") != text:
                raise ValueError(f"{case_id}: recovery changed the fixture on disk")
            recovered.append(case_id)
    return {"previous_result_id": unchanged, "unsaved_recovery": recovered}


def main():
    return check_main(Diagnostics, __doc__, extra_checks=check_recovery)


if __name__ == "__main__":
    raise SystemExit(main())
