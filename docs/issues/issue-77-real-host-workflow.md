# Issue #77 — Protected real-host validation

## Repository-side completion

Implemented on the repository branch:

- manually dispatched workflow with the exact six self-hosted labels;
- protected environment `o3k-real-host-validation` (maintainer approval must
  be configured in GitHub settings);
- canonical-repository and untrusted-fork guard before lifecycle execution;
- redacted pre-run/post-run result and unconditional artifact upload;
- portable guard, redaction, skipped-result, and workflow-shape tests;
- [ADR-0084](../adr/ADR-0084-protected-real-host-validation.md).

## Host-gated acceptance

This change does not claim that the host gate passed. A maintainer must
configure the protected environment and a runner with the exact labels,
QEMU/KVM, libvirt `qemu:///system`, required tools, an installed TestLab
profile, credentials, and a local CirrOS image. A manual run must produce
`status: passed` for the lifecycle and redacted result before real-host
evidence is accepted by the release gate.
