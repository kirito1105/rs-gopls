#!/usr/bin/env python3
"""Benchmark rs-gopls pull diagnostics and align verified GoLand timings."""

from bench_common import compare_main
from check_goland_diagnostics import Diagnostics


if __name__ == "__main__":
    raise SystemExit(compare_main(Diagnostics, __doc__))
