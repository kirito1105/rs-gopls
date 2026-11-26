#!/usr/bin/env python3
"""Verify navigation, symbol, and documentation updates across Go workspace changes."""

import argparse
import math
from pathlib import Path
import subprocess
import sys
import tempfile

from workspace_checks import discover_cases

# Re-export the helpers and cases available from this entry point before the split.
from check_goland_definitions import normalize_locations
from check_goland_references import Location
from check_goland_symbols import DOCUMENT, WORKSPACE, contains, lsp_span, request_document
from lsp_harness import (
    GO_LIST_RELAY, LspClient, Site, acknowledge_file_watches, executable, expect_error,
    go_list_relay, next_message, notify_changes, path_from_uri, response_detail,
    session, site, wait_for_response, wait_for_snapshot, write,
)
from workspace_checks._assertions import (
    check_location, check_references, check_type_location, expect_location, expect_outline,
    expect_target, expect_workspace_symbols, no_definition, reference_params, send_definition,
)
from workspace_checks.definition import (
    check_overlay_disk_restore, check_overlay_new_file, check_test_packages, check_type_overlay,
)
from workspace_checks.graph_refresh import check_async_refresh, check_go_work, check_gopath, check_mutations
from workspace_checks.hover_signature import (
    check_editor_help_overlays, check_editor_help_refresh, editor_help_params, expect_editor_hover,
    expect_editor_signature,
)
from workspace_checks.implementation import (
    IMPLEMENTATION_METHOD, check_active_implementation_cancel, check_implementation_cancel,
    check_implementation_candidate_packages, check_implementation_reverse_candidates,
    check_implementation_reverse_watched_sources, check_implementation_updates,
    check_implementations, implementation_params,
)
from workspace_checks.references import (
    check_active_reference_cancel, check_reference_cancel, check_reference_locals, check_reference_workspace,
)
from workspace_checks.symbols import (
    check_active_workspace_symbols, check_no_extra_symbol_responses, check_symbol_updates,
    check_symbols_waiting_for_graph, symbol_responses, workspace_symbol_request,
)


_REGISTRY = discover_cases()
CASES = {name: case.check for name, case in _REGISTRY.items()}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=executable)
    parser.add_argument("--case", choices=tuple(CASES), action="append", help="run only this case; may repeat")
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument(
        "--module", choices=sorted({case.module for case in _REGISTRY.values()}),
        action="append", metavar="MODULE", help="run only cases from this module; may repeat (%(choices)s)",
    )
    args = parser.parse_args(argv)
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be positive and finite")

    names = dict.fromkeys(args.case or CASES)
    if args.module:
        modules = set(args.module)
        names = [name for name in CASES if name in names and _REGISTRY[name].module in modules]
        if not names:
            parser.error("--case and --module select no cases")

    failures = 0
    with tempfile.TemporaryDirectory(prefix="rs-gopls-workspace-check-") as directory:
        root = Path(directory).resolve()
        for name in names:
            try:
                CASES[name](args.candidate, root / name, args.timeout)
            except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
                print(f"FAIL {name}: {exc}", file=sys.stderr)
                failures += 1
            else:
                print(f"PASS {name}")
    return int(bool(failures))


if __name__ == "__main__":
    raise SystemExit(main())
