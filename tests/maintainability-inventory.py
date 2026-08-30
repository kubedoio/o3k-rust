#!/usr/bin/env python3
"""Regression tests for the maintainability inventory's Rust scope model."""

import importlib.util
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "maintainability-inventory.py"
SPEC = importlib.util.spec_from_file_location("maintainability_inventory", SCRIPT)
INVENTORY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INVENTORY)


def assert_scope(source, expected, path="crates/example/src/lib.rs"):
    lines = source.splitlines()
    actual = INVENTORY.classify_rust_lines(lines, INVENTORY.is_dedicated_test_file(path))
    assert actual == expected, f"scope mismatch: {actual!r} != {expected!r}"


def safety(source, path="crates/example/src/lib.rs"):
    return INVENTORY.safety_occurrences(path, source.splitlines())


# CASE 1: item-level cfg(test) import does not consume following production.
assert_scope(
    "#[cfg(test)]\nuse test_support::Thing;\nfn production() {}",
    [True, True, False],
)

# CASE 2: a cfg(test) function ends at its own body, not at the next item.
assert_scope(
    "#[cfg(test)]\nfn test_only() { if true { let _ = 1; } }\nfn production() {}",
    [True, True, False],
)

# CASE 3: inline test modules remain test-only through nested function blocks.
assert_scope(
    "#[cfg(test)]\nmod tests {\n    fn first() { if true { let _ = 1; } }\n    fn second() { let _ = { 2 }; }\n}\nfn production() {}",
    [True, True, True, True, True, False],
)

# Multiple attributes between cfg(test) and the item remain part of its scope.
assert_scope(
    "#[cfg(test)]\n#[allow(dead_code)]\nfn test_only() {}\nfn production() {}",
    [True, True, True, False],
)

# cfg(any(test, ...)) is not test-only: it can compile in a production build.
assert_scope(
    '#[cfg(any(test, feature = "extra"))]\nfn maybe_production() {}\nfn production() {}',
    [False, False, False],
)
assert_scope(
    '#[cfg(all(test, feature = "extra"))]\nfn test_only() {}\nfn production() {}',
    [True, True, False],
)
assert_scope(
    "fn borrowed<'a>(value: &'a str) -> &'a str { value }\n"
    "#[cfg(test)]\nmod tests { fn nested() {} }\n"
    "fn production() {}",
    [False, True, True, False],
)

# CASES 4/5: production safety occurrences are detected, test-only ones are not.
production = safety("fn production() { let _ = Result::<(), ()>::Err(()).unwrap(); panic!(); }\n")
assert len(production["production_unwrap"]) == 1
assert len(production["production_panic"]) == 1
test_only = safety("#[cfg(test)]\nfn test_only() { let _ = None::<()>.unwrap(); panic!(); }\n")
assert not test_only["production_unwrap"]
assert not test_only["production_panic"]

# CASES 6/7: allow overrides have the same production/test distinction.
assert len(safety("#[allow(dead_code)]\nfn production() {}\n")["production_allow_overrides"]) == 1
assert not safety("#[cfg(test)]\n#[allow(dead_code)]\nfn test_only() {}\n")["production_allow_overrides"]

# CASE 8: mixed production and test items remain independently classified.
mixed = safety(
    "fn first() { let _ = 1; }\n"
    "#[cfg(test)]\nmod tests { fn nested() { panic!(); } }\n"
    "fn second() { let _ = 2; }\n"
)
assert not mixed["production_panic"]

# Dedicated test files are test-only regardless of inline brace shape.
assert all(INVENTORY.classify_rust_lines(["fn test() { panic!(); }"] , True))

print("maintainability inventory cfg(test) regression tests passed")
