#!/usr/bin/env python3
"""Fail closed on O3K core/application architecture boundary drift.

This is a ratchet, not a claim that the current architecture is already clean.
Known temporary exceptions live in contracts/core-architecture-boundaries.toml.
The exception set must match current debt exactly, so deleting debt requires
removing the matching exception in the same change and cannot leave a dormant
allowlist entry that permits later reintroduction.
"""

from __future__ import annotations

import argparse
import sys
import tomllib
from pathlib import Path


def fail(errors: list[str], message: str) -> None:
    errors.append(message)


def load_manifest(root: Path, crate: str) -> tuple[Path, dict]:
    manifest = root / "crates" / crate / "Cargo.toml"
    if not manifest.is_file():
        raise FileNotFoundError(
            f"missing application/domain manifest: {manifest.relative_to(root)}"
        )
    with manifest.open("rb") as handle:
        return manifest, tomllib.load(handle)


def dependency_names(manifest: dict) -> set[str]:
    return set(manifest.get("dependencies", {}).keys())


def rust_files(root: Path, crate: str) -> list[Path]:
    src = root / "crates" / crate / "src"
    if not src.is_dir():
        return []
    return sorted(src.rglob("*.rs"))


def check(root: Path) -> list[str]:
    contract_path = root / "contracts" / "core-architecture-boundaries.toml"
    errors: list[str] = []
    debt_notes: list[str] = []

    if not contract_path.is_file():
        return [f"missing architecture contract: {contract_path.relative_to(root)}"]

    with contract_path.open("rb") as handle:
        contract = tomllib.load(handle)

    if contract.get("schema_version") != 1:
        fail(errors, "architecture contract schema_version must be 1")

    applications = contract.get("application_crates", [])
    if not applications or len(applications) != len(set(applications)):
        fail(errors, "application_crates must be non-empty and unique")

    domain = contract.get("domain", {})
    domain_crate = domain.get("crate")
    allowed_domain_dependencies = set(domain.get("allowed_dependencies", []))
    forbidden_domain_markers = domain.get("forbidden_source_markers", [])

    if not domain_crate:
        fail(errors, "domain.crate is required")
    else:
        try:
            _, domain_manifest = load_manifest(root, domain_crate)
        except FileNotFoundError as exc:
            fail(errors, str(exc))
        else:
            actual_domain_dependencies = dependency_names(domain_manifest)
            unexpected = sorted(actual_domain_dependencies - allowed_domain_dependencies)
            if unexpected:
                fail(
                    errors,
                    f"{domain_crate} gained outward/unapproved dependencies: "
                    + ", ".join(unexpected),
                )

            for path in rust_files(root, domain_crate):
                text = path.read_text(encoding="utf-8")
                for marker in forbidden_domain_markers:
                    if marker in text:
                        fail(
                            errors,
                            f"{path.relative_to(root)} contains forbidden domain marker {marker!r}",
                        )

    application = contract.get("application", {})
    hard_forbidden = set(application.get("hard_forbidden_dependencies", []))
    ratcheted_dependencies = set(application.get("ratcheted_adapter_dependencies", []))
    adapter_debt = application.get("adapter_dependency_debt", {})
    concrete_store_symbol = application.get("concrete_store_symbol")
    concrete_store_debt_files = set(application.get("concrete_store_debt_files", []))

    unknown_debt_crates = sorted(set(adapter_debt) - set(applications))
    if unknown_debt_crates:
        fail(
            errors,
            "adapter_dependency_debt names non-application crates: "
            + ", ".join(unknown_debt_crates),
        )

    for crate, allowed in adapter_debt.items():
        outside_ratcheted = sorted(set(allowed) - ratcheted_dependencies)
        if outside_ratcheted:
            fail(
                errors,
                f"{crate} debt list contains dependencies not classified as ratcheted: "
                + ", ".join(outside_ratcheted),
            )

    observed_store_debt_files: set[str] = set()

    for crate in applications:
        try:
            _, manifest = load_manifest(root, crate)
        except FileNotFoundError as exc:
            fail(errors, str(exc))
            continue

        deps = dependency_names(manifest)
        forbidden_present = sorted(deps & hard_forbidden)
        if forbidden_present:
            fail(
                errors,
                f"{crate} has forbidden adapter/framework dependencies: "
                + ", ".join(forbidden_present),
            )

        declared_debt = set(adapter_debt.get(crate, []))
        observed_debt = deps & ratcheted_dependencies
        new_debt = sorted(observed_debt - declared_debt)
        stale_debt = sorted(declared_debt - observed_debt)
        if new_debt:
            fail(
                errors,
                f"{crate} gained new ratcheted adapter dependencies: {', '.join(new_debt)}",
            )
        if stale_debt:
            fail(
                errors,
                f"{crate} has stale architecture exceptions that must be removed: "
                + ", ".join(stale_debt),
            )
        if observed_debt:
            debt_notes.append(
                f"{crate}: adapter dependency debt = {', '.join(sorted(observed_debt))}"
            )

        if concrete_store_symbol:
            for path in rust_files(root, crate):
                text = path.read_text(encoding="utf-8")
                if concrete_store_symbol in text:
                    rel = path.relative_to(root).as_posix()
                    observed_store_debt_files.add(rel)
                    if rel not in concrete_store_debt_files:
                        fail(
                            errors,
                            f"new concrete-store coupling: {rel} contains {concrete_store_symbol}",
                        )

    missing_debt_paths = sorted(
        path for path in concrete_store_debt_files if not (root / path).is_file()
    )
    if missing_debt_paths:
        fail(
            errors,
            "architecture debt contract names missing files: " + ", ".join(missing_debt_paths),
        )

    stale_store_exceptions = sorted(concrete_store_debt_files - observed_store_debt_files)
    if stale_store_exceptions:
        fail(
            errors,
            "stale concrete-store exceptions must be removed: "
            + ", ".join(stale_store_exceptions),
        )

    for rel in sorted(observed_store_debt_files):
        debt_notes.append(f"{rel}: concrete store debt = {concrete_store_symbol}")

    if not errors:
        print("Architecture boundary check passed.")
        if debt_notes:
            print("Known migration debt (ratcheted; exact and temporary):")
            for note in sorted(set(debt_notes)):
                print(f"  - {note}")

    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
        help="repository root (defaults to the checkout containing this script)",
    )
    args = parser.parse_args()
    root = args.root.resolve()

    errors = check(root)
    if errors:
        print("Architecture boundary check FAILED:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        print(
            "\nDo not widen or leave dormant debt exceptions as a convenience. "
            "Refactor toward the inward dependency model, remove stale exceptions, "
            "or obtain explicit architecture review for a justified contract change.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
