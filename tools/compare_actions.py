#!/usr/bin/env python3
"""Benchmark rs-gopls formatting and code actions against verified GoLand timings."""

from bench_common import compare_main
from check_goland_actions import Actions


if __name__ == "__main__":
    raise SystemExit(compare_main(Actions, __doc__))
