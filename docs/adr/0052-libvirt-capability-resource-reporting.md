# ADR-0052: Report libvirt compute capacity through the agent contract

Status: Accepted

## Context

The local libvirt adapter already queried `virNodeInfo` and exposed memory,
architecture, versions, machine types, and lifecycle operations. Its protocol
projection nevertheless reported `max_vcpus` as zero and did not expose
whether the detected hypervisor was usable KVM. A zero CPU inventory prevents
Placement from selecting an otherwise usable host, while an implicit KVM
assumption makes readiness and scheduling evidence ambiguous.

## Decision

Capture `NodeInfo.cpus` as `total_vcpus` and project it to
`Capabilities.max_vcpus`. Project the adapter's KVM detection as a bounded
`CapabilityFlag` named `kvm`. Keep memory in MiB and preserve zero values when
libvirt cannot provide a resource value; later Placement code remains
responsible for reservations and allocation ratios.

## Consequences

The compute agent can report usable host capacity to the control plane without
opening libvirt from `o3kd`. Unit tests cover the projection, while actual
resource values and KVM usability still require a trusted libvirt host.
