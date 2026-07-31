# ADR-0084 — Protect and honestly report real-host validation

## Status

Accepted

## Context

Real-host TestLab validation needs a self-hosted Linux runner with KVM and
libvirt access. It can create guests and host networking resources, so a fork,
untrusted event, or concurrent run must not reach those commands. Portable CI
cannot prove those capabilities, and missing prerequisites must not become
release evidence.

## Decision

Add a manually dispatched workflow using exactly the labels `self-hosted`,
`linux`, `x64`, `kvm`, `libvirt`, and `o3k-testlab`. It uses the protected
`o3k-real-host-validation` environment; repository settings must require
maintainer approval for that environment. It grants `contents: read`, uses a
non-canceling fixed concurrency group, and accepts only
`kubedoio/o3k-rust` `workflow_dispatch` runs with no fork refs.

The pre-run guard runs before the lifecycle step and checks trust context,
required tools, `/dev/kvm`, and `qemu:///system`. It records bounded OS,
kernel, capability, and tool-presence metadata without reading or uploading
environment variables. The post-run guard always runs, merges the lifecycle
result, and exits nonzero unless prerequisites and lifecycle both pass. Both
guards write the redacted machine-readable `real-host-workflow-result.json`.
The workflow always uploads the artifact directory.

The privileged lifecycle remains the existing public OpenStack CLI harness.
No reset, uninstall, broad process kill, or recursive cleanup is added by the
workflow; existing cleanup remains scoped to resources created by that run.

## Consequences

Forks and unapproved/untrusted contexts fail closed before host operations.
Missing tools or KVM produce explicit skipped/non-passing evidence, while
workflow and lifecycle failures produce failed evidence. A protected GitHub
environment and correctly labeled runner are operational prerequisites, not
claims established by repository tests.
