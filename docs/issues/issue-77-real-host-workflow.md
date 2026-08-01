# Issue #77 — Protected real-host validation

## Repository-side completion

Implemented on the repository branch:

- manually dispatched workflow with the exact six self-hosted labels;
- pinned CirrOS 0.6.3 x86_64 image download with SHA-256 verification on the
  protected runner, so dispatch does not depend on an operator-provided local
  image path;
- idempotent protected-runner bootstrap for the OpenStack CLI virtualenv,
  `genisoimage`, the non-root Actions account, protected-path marker, and
  existing O3K bootstrap credential discovery;
- protected environment `o3k-real-host-validation` (maintainer approval must
  be configured in GitHub settings);
- canonical-repository and untrusted-fork guard before lifecycle execution;
- read-only pre-run owned-resource baseline and post-run leak comparison for
  O3K-owned libvirt domains and safely queryable TestLab OpenStack resources;
- redacted pre-run/post-run result and unconditional artifact upload;
- explicit 14-day retention for the complete redacted artifact bundle;
- portable guard, redaction, skipped-result, and workflow-shape tests;
- [ADR-0084](../adr/ADR-0084-protected-real-host-validation.md).

## Host-gated acceptance

This change does not claim that the host gate passed. A maintainer must
configure the protected environment and a runner with the exact labels,
  QEMU/KVM, libvirt `qemu:///system`, required tools, an installed TestLab
  profile, credentials, and access to the pinned CirrOS download. The workflow
  provisions only disposable CLI/config-drive dependencies; it does not guess
  or rotate the daemon credential. A manual run must produce
`status: passed` for the lifecycle and redacted result before real-host
evidence is accepted by the release gate. The baseline must be clean and the
post-run inventory must contain no added O3K-owned resources; the guards do not
delete anything.
