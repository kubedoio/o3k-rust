# Issue #76 — Protected runner capability and health probe

## Repository-side completion

Implemented on the repository branch:

- read-only `runner-capabilities.json` probe with redaction and honest
  `passed`/`skipped`/`failed` statuses;
- exact tool, `/dev/kvm`, `qemu:///system`, config-drive, disk-space,
  non-root/service-account, image-input, and runner-label checks;
- portable fake-command tests;
- protected workflow preflight and unconditional artifact upload;
- [ADR-0085](../adr/ADR-0085-runner-capability-probe.md).

## Host-gated acceptance

This change does not claim host acceptance. A maintainer must configure the
protected `o3k-real-host-validation` environment and an exact-label,
dedicated non-root runner with KVM/libvirt, the required tools, sufficient
disk, the configured service account, and a valid local image. A manual run
must produce `status: passed` in `runner-capabilities.json`; any other or
missing status blocks the pre-run guard. It must then produce a passing
redacted real-host workflow result with no owned-resource leaks. Missing host
capabilities remain skipped evidence; unsafe runner configuration fails the
probe. The probe makes no provisioning or destructive changes.
