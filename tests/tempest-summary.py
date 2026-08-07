#!/usr/bin/env python3
"""JUnit XML -> tempest-cinder-summary.json conversion for the Tempest gates.

Single implementation shared by:
  - tests/tempest-cinder-subset.sh  (real execution evidence)
  - tests/tempest-preflight.sh       (evidence-pipeline check)
  - tests/tempest-summary-generator.sh (regression test)

Honesty contract: a missing, malformed, or zero-test JUnit result is never
reported as useful evidence. The produced summary always carries an explicit
status:
  passed        all selected tests ran and none failed/errored
  failed        at least one selected test failed or errored
  harness-error the harness produced no usable JUnit result (missing file,
                malformed XML, or zero executed tests)
"""

import argparse
import json
import sys
import xml.etree.ElementTree as ET


def parse_junit(path):
    """Return (tests, failures, errors, skipped, test_cases, skip_reasons).

    On any parse failure return None so callers can emit an honest
    harness-error summary instead of a fabricated one.
    """
    if not path or not __import__("os").path.exists(path):
        return None
    try:
        root = ET.parse(path).getroot()
    except Exception:
        return None
    tests = int(root.attrib.get("tests", 0))
    failures = int(root.attrib.get("failures", 0))
    errors = int(root.attrib.get("errors", 0))
    skipped = int(root.attrib.get("skipped", 0))
    test_cases = []
    skip_reasons = {}
    for case in root.iter("testcase"):
        name = case.attrib.get("name", "")
        status = "passed"
        reason = ""
        if case.find("failure") is not None:
            status = "failed"
        elif case.find("error") is not None:
            status = "error"
        elif case.find("skipped") is not None:
            status = "skipped"
            skip = case.find("skipped")
            reason = (skip.attrib.get("message") or skip.text or "").strip()
        test_cases.append({"name": name, "status": status})
        if status == "skipped" and reason:
            skip_reasons[name] = reason
    return tests, failures, errors, skipped, test_cases, skip_reasons


def build_summary(junit_path, revision, plugin_pin, selected_ids,
                  o3k_commit="", cinder_version="", status_hint=None):
    parsed = parse_junit(junit_path)
    base = {
        "artifact_type": "tempest-cinder-summary.json",
        "evidence_tier": "tempest",
        "tempest_revision": revision,
        "cinder_tempest_plugin": plugin_pin,
        "selected_test_ids": [tid for tid in selected_ids if tid],
        "profile_id": "openstack-service-testbed",
        "o3k_commit": o3k_commit,
        "cinder_version": cinder_version,
        "test_cases": [],
        "skip_reasons": {},
        "passed": 0,
        "failed": 0,
        "skipped": 0,
    }
    if parsed is None:
        base.update({
            "status": status_hint or "harness-error",
            "reason": "no usable JUnit result was produced",
        })
        return base
    tests, failures, errors, skipped, test_cases, skip_reasons = parsed
    if tests <= 0:
        base.update({
            "status": status_hint or "harness-error",
            "reason": "zero Tempest tests were executed; not useful evidence",
            "test_cases": [],
            "skip_reasons": {},
        })
        return base
    executed = max(tests - failures - errors - skipped, 0)
    base.update({
        "status": "failed" if (failures + errors) > 0 else "passed",
        "passed": executed,
        "failed": failures + errors,
        "skipped": skipped,
        "test_cases": test_cases,
        "skip_reasons": skip_reasons,
    })
    return base


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--junit", required=True, help="path to JUnit XML")
    parser.add_argument("--out", required=True, help="output summary JSON path")
    parser.add_argument("--revision", required=True, help="tempest revision")
    parser.add_argument("--plugin", required=True,
                        help="cinder-tempest-plugin revision")
    parser.add_argument("--selected", required=True,
                        help="comma-separated allowlisted test ids")
    parser.add_argument("--o3k-commit", default="", help="source commit")
    parser.add_argument("--cinder-version", default="",
                        help="recorded Cinder version")
    parser.add_argument("--status-hint", default=None,
                        help="forced status for harness failures")
    args = parser.parse_args(argv)
    selected = [tid for tid in args.selected.split(",") if tid]
    summary = build_summary(
        args.junit, args.revision, args.plugin, selected,
        o3k_commit=args.o3k_commit, cinder_version=args.cinder_version,
        status_hint=args.status_hint)
    with open(args.out, "w", encoding="utf-8") as stream:
        json.dump(summary, stream, indent=2)
        stream.write("\n")
    if summary["status"] == "harness-error":
        return 2
    if summary["status"] == "failed":
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
