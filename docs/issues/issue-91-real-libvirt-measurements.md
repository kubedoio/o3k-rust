# Issue #91 — Real-libvirt measurement evidence

## Repository slice

The release gate previously freshness-checked only the benchmark summary. This
slice adds a timestamp to the raw benchmark artifact, validates raw freshness,
requires the raw and summary timestamps to be identical, and requires both
artifacts to explicitly declare release eligibility. The raw digest still
binds the summary to the exact measured document, and the release gate
recomputes target results from those raw measurements instead of trusting a
truthy summary map.

## Evidence and non-claims

This is an evidence-integrity improvement only. It does not claim a real host,
guest, libvirt, QEMU, or CirrOS measurement. The existing harness remains
portable and explicitly marks fake or incomplete profiles as non-release
evidence.

## Acceptance coverage

- raw and summary benchmark timestamps are emitted together;
- stale raw evidence is rejected even when its summary hash is refreshed;
- mismatched raw/summary timestamps are rejected;
- ineligible raw/summary benchmark evidence is rejected;
- a summary with forged target results is rejected even when its raw digest is
  valid;
- real-host measurements remain pending until the protected CirrOS workflow
  produces the required raw samples and environment metadata.

## Decision

See [ADR-0098](../adr/ADR-0098-benchmark-raw-freshness.md).
