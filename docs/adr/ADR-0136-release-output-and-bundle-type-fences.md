# ADR-0136 — Fence release output roots and bundle file types

Status: accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: governance

Release packaging rejects a configured `dist` root that is a symlink or other
special file before invoking Cargo or removing an output directory. The root
may be selected with `O3K_RELEASE_DIST_DIR` for isolated packaging tests, but
it must resolve to a real directory.

Bundle verification now rejects every filesystem entry that is neither a
directory nor a regular file (in addition to rejecting symlinks). This keeps
the checksum manifest complete and prevents FIFOs, sockets, device nodes, or
other unchecked entries from being smuggled into a release bundle.

These are packaging-integrity fences only; they do not claim reproducible
builds, signed publication, host evidence, or independent human approval.
