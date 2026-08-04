# ADR-0098 — Require fresh raw benchmark evidence

Status: Accepted for the issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: governance

#91 measurement and release-evidence boundary.

## Context

The release gate checked the summary artifact's `finished_at` timestamp, while
the raw benchmark document was validated only by content hash and shape. An old
raw measurement could therefore be paired with a newly timestamped summary and
satisfy the summary freshness check. The hash proves which raw document was
reviewed, but not when that document was produced.

## Decision

The measurement harness writes the same positive Unix epoch `finished_at` into
both `raw.json` and `summary.json`. The release gate applies its existing
future and maximum-age policy to the raw artifact as well and requires the
summary and raw timestamps to match exactly. The raw digest remains the binding
between the two files.

## Consequences

An old raw measurement cannot be refreshed by editing only its summary. A
reviewer receives a single freshness timestamp for the measured data and its
evaluation. Existing fake-profile measurements remain diagnostic and are still
ineligible for real-libvirt release evidence.

## Non-goals

This does not establish trusted time, sign artifacts, identify the host, or
produce real guest/libvirt measurements. Those still require the protected
real-host workflow and the complete CirrOS profile.

## Verification

`tests/release-gate.sh` includes a stale-raw regression that recomputes the
summary hash while leaving the raw timestamp outside the allowed age window;
the gate must reject it.
