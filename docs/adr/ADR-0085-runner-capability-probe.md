# ADR-0085 — Read-only protected runner capability probe

## Status

Accepted

## Context

The protected real-host workflow needs evidence that its selected runner is
eligible before it reaches host-mutating TestLab commands. Runner labels and
host prerequisites are operational inputs; repository CI cannot establish that
a real host has accepted the workflow. A probe must also avoid turning missing
host capability into a false failure or exposing credentials in an artifact.

## Decision

Add `scripts/real-host-capability-probe.sh`. It performs only read-only checks
and writes the redacted, machine-readable
`runner-capabilities.json`. The required checks are `virsh`, `qemu-img`, `ip`,
`dnsmasq`, `openstack`, and `xorriso` for deterministic config-drive ISO
materialization, a readable character device at exact path `/dev/kvm`, a
successful `virsh -c qemu:///system uri`,
minimum free disk space, non-root execution, a declared service account, the
exact six runner labels, and a declared absolute regular image path.

The artifact uses `passed` only when every check passes, `skipped` when a host
is ineligible or a prerequisite is absent, and `failed` for malformed or
unsafe configuration. It contains bounded reason codes and booleans/countable
disk values only. It never records command output, environment variables,
credentials, arbitrary command paths, or image contents. The probe does not
install packages, alter services, contact OpenStack, create resources, or
clean up resources. The protected workflow runs it before the pre-run guard,
uses a passed artifact as the guard's eligibility input, and uploads it with
the other artifacts even when a later step fails. A missing, malformed,
skipped, or failed artifact makes the pre-run guard write an explicit redacted
blocked reason and exit nonzero; it can never allow the lifecycle step.

The label and service-account declarations are explicit workflow inputs to the
probe. GitHub runner assignment remains enforced by the workflow's exact
`runs-on` labels and protected environment; the artifact is not a claim that
GitHub settings or host acceptance have been configured.

## Consequences

Portable fake-command tests can exercise the serialization and failure paths
without requiring KVM or libvirt. A maintainer still must configure the
protected environment and a dedicated non-root service account on a real
runner, then inspect a passing artifact and lifecycle result before accepting
host evidence.

## Non-goals

This probe is not provisioning, remediation, security attestation, a libvirt
functional test, or host acceptance.
