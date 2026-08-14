#!/usr/bin/env python3
"""Generate a candidate evidence manifest from already-bound JSON artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
from pathlib import Path

SHA256 = re.compile(r"[0-9a-f]{64}\Z")
REQUIRED = (
    "real_libvirt_e2e",
    "clean_ubuntu_install",
    "clean_debian_install",
    "failure_recovery",
    "benchmark",
    "benchmark_raw",
)


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def parse_artifact(value: str) -> tuple[str, Path]:
    logical, separator, path = value.partition("=")
    if not separator or logical not in REQUIRED or not path:
        raise argparse.ArgumentTypeError(
            f"artifact must be LOGICAL=PATH, where LOGICAL is one of {', '.join(REQUIRED)}"
        )
    return logical, Path(path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate-sha", required=True)
    parser.add_argument("--o3kd-sha256", required=True)
    parser.add_argument("--o3k-compute-sha256", required=True)
    parser.add_argument("--bundle-tree-sha256", required=True)
    parser.add_argument("--artifact", action="append", type=parse_artifact, required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    if not re.fullmatch(r"[0-9a-f]{40}", args.candidate_sha):
        parser.error("candidate SHA must be a 40-character lowercase commit SHA")
    for name in ("o3kd_sha256", "o3k_compute_sha256", "bundle_tree_sha256"):
        if not SHA256.fullmatch(getattr(args, name)):
            parser.error(f"{name.replace('_', '-')} must be a lowercase SHA-256 digest")

    artifacts = dict(args.artifact)
    missing = [name for name in REQUIRED if name not in artifacts]
    if missing:
        parser.error("missing required artifacts: " + ", ".join(missing))

    entries: dict[str, dict[str, object]] = {}
    errors: list[str] = []
    for logical in REQUIRED:
        path = artifacts[logical].resolve()
        try:
            document = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            errors.append(f"{logical}: cannot read JSON artifact ({error})")
            continue
        if not isinstance(document, dict):
            errors.append(f"{logical}: artifact root must be an object")
            continue
        expected = {
            "source_commit": args.candidate_sha,
            "o3kd_sha256": args.o3kd_sha256,
            "o3k_compute_sha256": args.o3k_compute_sha256,
            "bundle_tree_sha256": args.bundle_tree_sha256,
        }
        for field, value in expected.items():
            if document.get(field) != value:
                errors.append(f"{logical}: {field} must match the candidate")
        entries[logical] = {
            "artifact": os.path.relpath(path, args.output.parent.resolve()),
            "artifact_sha256": digest(path),
            **expected,
            "finished_at": document.get("finished_at"),
            "status": document.get("status"),
        }

    if errors:
        for error in errors:
            print(f"candidate-evidence-manifest: {error}", file=sys.stderr)
        return 1

    manifest = {
        "artifact_type": "candidate-evidence-manifest",
        "candidate_sha": args.candidate_sha,
        "o3kd_sha256": args.o3kd_sha256,
        "o3k_compute_sha256": args.o3k_compute_sha256,
        "bundle_tree_sha256": args.bundle_tree_sha256,
        "artifacts": entries,
        "all_exact_candidate": True,
        "all_binary_bound": True,
        "any_stale_ancestor_evidence_used": False,
        "errors": [],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"candidate evidence manifest written: {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
