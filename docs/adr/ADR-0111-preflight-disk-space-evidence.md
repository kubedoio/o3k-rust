# ADR-0111 — Fail closed on missing preflight disk-space evidence

Status: Accepted for the repository-side preparation of issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: governance

#90. This ADR does not
claim that a Debian host passed preflight or the full TestLab lifecycle.

## Context

`packaging/preflight.sh` requires at least 1 GiB free on `/var/lib`. Its
previous `awk` check rejected a low value, but an empty or malformed `df`
result could make the pipeline exit successfully without proving available
space. Treating missing evidence as success weakens the install safety gate.

## Decision

Require the second line of `df -Pk /var/lib` output to exist and contain an
integer available-blocks field before accepting the disk-space check. Missing
or malformed evidence fails closed; a valid value below 1 GiB remains a
failure. The check remains portable and does not require a Debian host.

## Non-goals

- no package-manager, filesystem, libvirt, systemd, or clean-host validation;
- no change to installer input validation owned by issue #89;
- no claim of clean Debian installation or TestLab lifecycle acceptance.

## Verification

`tests/packaging-safety.sh` uses a fake `df` that exits successfully without
output and verifies that preflight rejects the missing evidence.

## Provenance

This is an independently authored repository decision based on issue #90 and
the portable preflight contract. No private source or implementation was
used.
