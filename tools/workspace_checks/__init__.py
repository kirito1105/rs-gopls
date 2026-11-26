"""Discover workspace cases declared as (name, check[, old order]) tuples."""

from collections.abc import Callable
from dataclasses import dataclass
from importlib import import_module
import pkgutil


@dataclass(frozen=True)
class RegisteredCase:
    module: str
    check: Callable
    order: int | None


def discover_cases():
    cases = {}
    orders = {}
    modules = sorted(
        info.name for info in pkgutil.iter_modules(__path__)
        if not info.ispkg and not info.name.startswith("_")
    )
    for module in modules:
        for name, check, *legacy_order in getattr(import_module(f".{module}", __name__), "CASES", ()):
            order, = legacy_order or (None,)
            if name in cases:
                raise RuntimeError(f"duplicate workspace case {name!r} in modules {cases[name].module!r} and {module!r}")
            if order is not None:
                if order in orders:
                    other = orders[order]
                    raise RuntimeError(
                        f"duplicate workspace case order {order} for "
                        f"{cases[other].module}.{other} and {module}.{name}"
                    )
                orders[order] = name
            cases[name] = RegisteredCase(module, check, order)
    return dict(sorted(cases.items(), key=lambda item: (item[1].order is None, item[1].order)))
