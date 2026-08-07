#!/usr/bin/env python3
"""Emit an exact-source binary build manifest (preparation for the
pre-built-artifact pipeline follow-up, issue #502).

Records the source commit, rustc version, and sha256 of the two protected-run
binaries as produced by ordinary CI. This is measurement/preparation only: the
protected runner does not trust it and continues to verify its own build
against the exact checked-out source until the accepted follow-up decides
otherwise.
"""

import hashlib
import json
import os
import subprocess
import sys


def main(argv):
    if len(argv) != 3:
        raise SystemExit("usage: emit-build-manifest.py <manifest-path> <source-commit>")
    manifest_path, commit = argv[1:]
    entries = {}
    for name in ("o3kd", "o3k-compute-bin"):
        path = os.path.join("target", "debug", name)
        if not os.path.exists(path):
            raise SystemExit("%s binary missing (build it first)" % name)
        digest = hashlib.sha256(open(path, "rb").read()).hexdigest()
        entries[name] = {"sha256": digest, "binary": path}
    rustc = subprocess.run(
        ["rustc", "--version"], capture_output=True, text=True,
        check=False).stdout.strip()
    doc = {
        "artifact_type": "build-manifest",
        "source_commit": commit,
        "rustc": rustc,
        "binaries": entries,
    }
    os.makedirs(os.path.dirname(manifest_path), exist_ok=True)
    with open(manifest_path, "w", encoding="utf-8") as stream:
        json.dump(doc, stream, indent=2)
        stream.write("\n")
    print("build manifest written to %s" % manifest_path)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
