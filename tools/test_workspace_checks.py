"""Lock down workspace case discovery and command-line selection."""

from contextlib import contextmanager, redirect_stderr, redirect_stdout
import importlib
from io import StringIO
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

import check_workspace_updates
import workspace_checks


LEGACY_CASES = (
    "mutations",
    "go-work",
    "test-packages",
    "gopath",
    "async-refresh",
    "overlay-new-file",
    "overlay-disk-restore",
    "type-overlay",
    "editor-help-overlays",
    "editor-help-refresh",
    "symbol-updates",
    "symbol-waiting-for-graph",
    "symbol-active-search",
    "reference-locals",
    "reference-workspace",
    "reference-cancel",
    "reference-active-cancel",
    "implementation-updates",
    "implementation-candidate-packages",
    "implementation-reverse-candidates",
    "implementation-reverse-watched-sources",
    "implementation-cancel",
    "implementation-active-cancel",
)


@contextmanager
def extra_modules(**sources):
    with tempfile.TemporaryDirectory() as directory:
        for module, source in sources.items():
            (Path(directory) / f"{module}.py").write_text(source, encoding="utf-8")
        with mock.patch.object(workspace_checks, "__path__", [*workspace_checks.__path__, directory]):
            importlib.invalidate_caches()
            try:
                yield
            finally:
                for module in sources:
                    sys.modules.pop(f"workspace_checks.{module}", None)
                    if hasattr(workspace_checks, module):
                        delattr(workspace_checks, module)


class WorkspaceDiscoveryTests(unittest.TestCase):
    def test_legacy_order_and_public_cases(self):
        cases = workspace_checks.discover_cases()
        self.assertEqual(tuple(cases), LEGACY_CASES)
        self.assertEqual(tuple(check_workspace_updates.CASES), LEGACY_CASES)
        for name, case in cases.items():
            self.assertIs(check_workspace_updates.CASES[name], case.check)
            self.assertIs(getattr(check_workspace_updates, case.check.__name__), case.check)

    def test_discovers_new_modules_and_preserves_declaration_order(self):
        with extra_modules(
            zz_extension='def check(*args): pass\nCASES = [("z-first", check), ("a-second", check, None)]',
            aa_extension='def check(*args): pass\nCASES = [("another-case", check)]',
            _private='raise AssertionError("private modules must not be imported")',
        ):
            cases = workspace_checks.discover_cases()
        self.assertEqual(tuple(cases), (*LEGACY_CASES, "another-case", "z-first", "a-second"))
        self.assertEqual(cases["another-case"].module, "aa_extension")
        self.assertEqual(cases["a-second"].module, "zz_extension")

    def test_duplicate_case_names_report_both_modules(self):
        with extra_modules(zz_duplicate='def check(*args): pass\nCASES = [("mutations", check)]'):
            with self.assertRaisesRegex(RuntimeError, "duplicate workspace case 'mutations'.*graph_refresh.*zz_duplicate"):
                workspace_checks.discover_cases()

    def test_duplicate_legacy_order_reports_both_modules_and_names(self):
        with extra_modules(zz_duplicate='def check(*args): pass\nCASES = [("conflicting-case", check, 1)]'):
            with self.assertRaisesRegex(
                RuntimeError, r"duplicate workspace case order 1.*graph_refresh\.mutations.*zz_duplicate\.conflicting-case",
            ):
                workspace_checks.discover_cases()


class WorkspaceCliTests(unittest.TestCase):
    def run_cli(self, *options, failure=None):
        calls = []

        def fake_case(name):
            def check(executable_path, root, timeout):
                calls.append((name, Path(executable_path), root, timeout))
                root.mkdir()
                if name == failure:
                    raise RuntimeError("injected failure")
            return check

        cases = {name: fake_case(name) for name in LEGACY_CASES}
        with mock.patch.dict(check_workspace_updates.CASES, cases), redirect_stdout(StringIO()) as output:
            with redirect_stderr(StringIO()) as errors:
                try:
                    code = check_workspace_updates.main([sys.executable, *options])
                except SystemExit as exc:
                    code = exc.code
        return code, output.getvalue().splitlines(), errors.getvalue(), calls

    def assert_selection(self, expected, *options):
        code, lines, errors, calls = self.run_cli(*options)
        self.assertEqual(code, 0, errors)
        self.assertEqual(lines, [f"PASS {name}" for name in expected])
        self.assertEqual([name for name, *_ in calls], list(expected))
        self.assertFalse(errors)
        return calls

    def test_default_runs_every_case_in_baseline_order(self):
        calls = self.assert_selection(LEGACY_CASES)
        self.assertEqual({binary for _, binary, _, _ in calls}, {Path(sys.executable).resolve()})
        self.assertEqual({timeout for _, _, _, timeout in calls}, {60.0})
        self.assertEqual(len({root for _, _, root, _ in calls}), len(LEGACY_CASES))
        self.assertTrue(all(name == root.name and not root.exists() for name, _, root, _ in calls))

    def test_repeated_case_keeps_the_first_occurrence_and_requested_order(self):
        calls = self.assert_selection(
            ("type-overlay", "test-packages"),
            "--case", "type-overlay", "--case", "test-packages", "--case", "type-overlay", "--timeout", "1.5",
        )
        self.assertEqual({timeout for _, _, _, timeout in calls}, {1.5})

    def test_repeated_modules_take_the_union_in_default_order(self):
        self.assert_selection(
            ("test-packages", "overlay-new-file", "overlay-disk-restore", "type-overlay",
             "symbol-updates", "symbol-waiting-for-graph", "symbol-active-search"),
            "--module", "symbols", "--module", "definition", "--module", "symbols",
        )

    def test_case_and_module_intersect_in_default_order(self):
        self.assert_selection(
            ("mutations", "go-work"),
            "--module", "graph_refresh", "--case", "go-work", "--case", "type-overlay",
            "--case", "mutations", "--case", "go-work",
        )

    def test_a_failing_case_does_not_skip_the_next_case(self):
        code, lines, errors, calls = self.run_cli("--case", "type-overlay", "--case", "go-work", failure="type-overlay")
        self.assertEqual(code, 1)
        self.assertEqual(lines, ["PASS go-work"])
        self.assertEqual(errors.splitlines(), ["FAIL type-overlay: injected failure"])
        self.assertEqual([name for name, *_ in calls], ["type-overlay", "go-work"])

    def test_invalid_arguments_return_two_without_running_any_case(self):
        for options, error in [
            (("--case", "unknown"), "invalid choice"),
            (("--module", "unknown"), "invalid choice"),
            (("--module", "symbols", "--case", "go-work"), "--case and --module select no cases"),
            *[(("--timeout", value), "--timeout must be positive and finite") for value in ("0", "-1", "inf", "nan")],
        ]:
            with self.subTest(options=options):
                code, lines, errors, calls = self.run_cli(*options)
                self.assertEqual(code, 2)
                self.assertIn(error, errors)
                self.assertEqual(lines, [])
                self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
