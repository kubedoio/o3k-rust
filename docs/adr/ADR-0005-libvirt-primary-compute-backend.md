# ADR-0005 — Libvirt/KVM as the primary compute backend

Status: Accepted

## Decision

O3K’s first real OpenStack-compatible TestLab release, `v0.2.0-alpha.1`, uses
standard QEMU/KVM through libvirt as its primary compute backend. The provider
abstraction remains the stable boundary, with the implementations:

```text
ComputeProvider
├── FakeComputeProvider
├── LibvirtComputeProvider   (default and release-critical)
└── CellHvComputeProvider    (optional)
```

The control plane (`o3kd`) owns OpenStack semantics, resource identity,
scheduling, placement, desired state, operation state, and reconciliation. It
does not connect directly to a libvirt socket, whether local or remote. A
dedicated `o3k-compute` agent runs on each compute host, owns bounded
infrastructure execution and observed provider state, and connects locally to
the system libvirt instance at `qemu:///system`.

The resulting execution boundary is:

```text
OpenStack CLI / SDK
        |
        v
      o3kd
        |
        | versioned provider protocol
        v
   o3k-compute
        |
        | local libvirt Unix socket: qemu:///system
        v
     QEMU/KVM
```

CellHV remains independently releasable behind the same provider contract. It
may be used for later profiles or comparative integration work, but it is not
required for, and must not block, the `v0.2.0-alpha.1` release.

## Context

The TestLab product promise is a reproducible OpenStack-compatible workflow
that can boot a real guest on a supported Linux host. A fake provider is useful
for fast development and contract tests, but it cannot establish real guest
boot, config-drive, networking, console output, or libvirt restart evidence.
Making CellHV the first real provider would also make the release depend on an
independently deployed system that is not required by the TestLab host model.

Libvirt provides the standard host-local boundary for QEMU/KVM. The separation
keeps privileged host operations out of the control plane and permits the
compute agent to report capabilities and reconcile provider state locally.

## Release exit criteria

`v0.2.0-alpha.1` is not complete merely because the libvirt adapter exists. The
release-critical evidence must show, on a clean supported Linux host, the
documented OpenStack CLI workflow:

1. authenticate through the Keystone-compatible API;
2. upload and inspect an image;
3. create a flat network and subnet and a Nova flavor;
4. create a real QEMU/KVM guest through `o3k-compute` and local libvirt;
5. deliver a deterministic fixed IP and config-drive/cloud-init metadata;
6. inspect, list, stop, start, reboot, console-log, and delete the guest;
7. restart `o3kd`, `o3k-compute`, and libvirt and reconcile without duplicate or
   lost instances;
8. remove every O3K-owned resource after deletion; and
9. reproduce the workflow using the documented install, test, reset, and
   uninstall commands.

The release gate must include executable tests and published evidence for the
real-libvirt path, failure/unknown-outcome recovery, compatibility status,
clean-host installation, resource cleanup, measured performance/footprint,
SBOM, checksums, provenance, and signed artifacts. CellHV coverage is reported
separately and is not substituted for missing libvirt evidence.

## Consequences

- Libvirt/KVM, image boot, placement, host networking, DHCP, config-drive, and
  Nova lifecycle work are ordered ahead of CellHV expansion.
- `o3k-compute` is the only component that may access the host’s system libvirt
  socket; the control plane communicates through the versioned provider
  protocol.
- The provider contract and ownership rules remain shared by fake, libvirt, and
  CellHV implementations.
- CellHV remains optional and independently releasable rather than being
  removed or made incompatible.
- A skipped real-libvirt environment is reported as missing evidence, not as a
  passing release result.
- Privileged operations, libvirt ownership metadata, cleanup, and failure
  recovery require explicit security and integration review.

## Public references

- libvirt URI documentation: <https://libvirt.org/uri.html>
- libvirt architecture documentation: <https://libvirt.org/architecture.html>
- O3K provider boundary: `docs/specs/SPEC-0003-provider-contract.md`

