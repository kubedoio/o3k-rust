# ADR-0104 — Verify image overlays before publication

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, image, governance

## Context

`qemu-img create` can exit successfully while the resulting temporary image
does not have the format or backing chain that the image cache expects. The
cache previously published that output after checking only the process exit
status, allowing a malformed or foreign-backed overlay to become a managed
artifact.

## Decision

After `qemu-img create`, inspect the temporary overlay with
`qemu-img info --output=json`. Publication is allowed only when the reported
format is exactly `qcow2`, at least one backing filename is present, and every
reported `backing-filename` or `full-backing-filename` resolves to the exact
canonical managed base path. Missing, malformed, failed, or mismatched
metadata fails closed and removes the temporary output before the final atomic
rename.

The executable path remains `qemu-img` by default; the private constructor
injection used by unit tests does not change the production command contract.

## Consequences

Malformed, raw, backing-less, and foreign-backed outputs are never published
as managed overlays. This adds one metadata query per newly-created overlay.
Existing final overlays retain the existing idempotent behavior and are not
revalidated by this bounded change.

The change is repository-side only. It does not claim Glance integration,
compute-agent image realization, or trusted real-host qemu-img evidence.

## Public sources

- QEMU `qemu-img` public command behavior and JSON information output,
  verified through the executable contract represented by the deterministic
  fake-qemu regression test.
- Rust standard library `std::fs::canonicalize`, accessed 2026-07-31.
