# Issue #91 — Real-libvirt measurement evidence

## Repository slice

The release gate previously freshness-checked only the benchmark summary. This
slice adds a timestamp to the raw benchmark artifact, validates raw freshness,
and requires the raw and summary timestamps to be identical. The raw digest
still binds the summary to the exact measured document.

## Evidence and non-claims

This is an evidence-integrity improvement only. It does not claim a real host,
guest, libvirt, QEMU, or CirrOS measurement. The existing harness remains
portable and explicitly marks fake or incomplete profiles as non-release
evidence.

## Acceptance coverage

- raw and summary benchmark timestamps are emitted together;
- stale raw evidence is rejected even when its summary hash is refreshed;
- mismatched raw/summary timestamps are rejected;
- real-host measurements remain pending until the protected CirrOS workflow
  produces the required raw samples and environment metadata.

## Decision

See [ADR-0098](../adr/ADR-0098-benchmark-raw-freshness.md).
