#!/usr/bin/env python3
"""Benchmark rs-gopls call and type hierarchies against verified GoLand timings."""

from bench_common import compare_main
from check_goland_hierarchy import Hierarchy


if __name__ == "__main__":
    raise SystemExit(compare_main(Hierarchy, __doc__))
