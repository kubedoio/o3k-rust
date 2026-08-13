# O3K

<p align="center">
  <strong>Rust-native Cloud Operating System with an OpenStack-compatible northbound surface.</strong><br />
  OpenStack compatibility northbound. O3K Cloud Kernel in the middle. Typed infrastructure execution southbound.
</p>

<p align="center">
  <img alt="Status: alpha" src="https://img.shields.io/badge/status-alpha-f59e0b" />
  <img alt="Implementation: Rust" src="https://img.shields.io/badge/implementation-Rust-111827" />
  <img alt="Kubernetes target" src="https://img.shields.io/badge/Kubernetes-first--class%20target-326ce5" />
  <img alt="License: Apache 2.0" src="https://img.shields.io/badge/license-Apache--2.0-3b82f6" />
</p>

![O3K Cloud Operating System architecture](docs/architecture/o3k-cloud-os.svg)

O3K is **not** a service-for-service Rust rewrite of Nova, Neutron, Keystone,
Glance, Placement, and Cinder. OpenStack service names define compatibility
surfaces; O3K owns its internal cloud model.

Core principles:

- **OpenStack compatibility is northbound.** Existing CLI/SDK/Terraform
  workflows remain valuable contracts.
- **O3K owns cloud authority.** Public IDs, ownership, desired state,
  scheduling, operations, and reconciliation are O3K concerns.
- **The Cloud Kernel is shared.** IAM, authorization, resource ownership,
  operations, quotas, audit/event identity, and failure semantics are reused by
  first-class O3K domains.
- **Execution is southbound.** Host agents/providers perform bounded mutations
  and report observations.
- **Kubernetes is a first-class deployment target.** Kubernetes may operate the
  O3K control plane, but it does not become O3K's VM scheduler, tenant-resource
  database, or Cloud Kernel.

> **Current status:** alpha. `v0.2.0-alpha.1` remains a Rust-native,
> OpenStack-compatible libvirt TestLab direction. Production HA, PostgreSQL,
> Kubernetes HA, native persistent volumes, and full OpenStack parity are not
> current support claims.

## What runs today

![O3K current runtime topology](docs/architecture/o3k-runtime-topology.svg)

```text
OpenStack clients
      |
    o3kd
      |
SQLite + O3K domain/scheduler/reconciler
      |
versioned provider boundary
      |
 o3k-compute
      |
libvirt / QEMU / KVM
```

`o3kd` is the current integrated control-plane composition shell. Host-local
real compute execution crosses the typed gRPC+mTLS agent boundary.

## Kubernetes-native target

Kubernetes deployability is a **main O3K product target**, not community
packaging added later.

The target architecture is:

```text
OpenStack / O3K clients
          |
   Gateway / Service
          |
+------------- Kubernetes -------------+
|  o3kd-1     o3kd-2     o3kd-3        |
|      \         |         /             |
|          PostgreSQL                    |
|   probes / rollout / config / metrics |
+----------------+----------------------+
                 |
           versioned mTLS
                 |
      external hypervisor hosts
                 |
            o3k-compute
                 |
         libvirt / QEMU / KVM
```

The governing rules are deliberately strict:

1. Kubernetes operates the **control-plane processes**; O3K remains the cloud
   authority.
2. Cloud Kernel/domain crates do not depend on Kubernetes APIs.
3. PostgreSQL is required before an HA/cloud-native Kubernetes support claim;
   SQLite remains the single-controller/TestLab store.
4. Multiple `o3kd` replicas require durable work ownership and controller
   fencing. Pod replication alone is not correctness.
5. Hypervisor/network/storage execution stays host-local by default instead of
   being forced into privileged pods.
6. Kubernetes CRDs may later manage the O3K installation, but do not become the
   canonical database for servers, networks, volumes, or operations.
7. Pod-local state is cache/scratch only for authoritative control-plane data.
8. OCI images + Helm are the first packaging target; an Operator is justified
   only when O3K-specific lifecycle automation needs one.

See [ADR-0167 — Kubernetes-native control-plane deployment](docs/adr/ADR-0167-kubernetes-native-control-plane-deployment.md).

## Durable control loop

![O3K durable control loop](docs/architecture/o3k-control-loop.svg)

```text
intent
-> authorization
-> durable desired state + operation
-> scheduling
-> provider command
-> infrastructure mutation
-> observation
-> reconciliation / compensation
```

A timeout is an **unknown outcome**, not proof of failure. O3K observes before
retrying an operation whose side effect may already have happened.

## OpenStack compatibility mapping

| OpenStack surface | O3K domain |
|---|---|
| Keystone | O3K IAM |
| Glance | O3K Image |
| Nova | O3K Compute |
| Neutron | O3K Network |
| Placement | O3K Capacity / Placement |
| Cinder | O3K Volume compatibility / hosted integration today |

## PostgreSQL direction

SQLite is the current minimal/TestLab default.

PostgreSQL is the production-oriented persistence target and a prerequisite for
the future HA Kubernetes profile. O3K will not use shared-SQLite or
distributed-filesystem workarounds as a shortcut to Kubernetes HA.

## Product profiles

O3K has one product architecture and three primary deployment/evidence profiles:

- OpenStack service testbed;
- native O3K TestLab/cloud;
- small edge cloud, initially targeting roughly 10–20 hypervisors.

Kubernetes is a deployment substrate target across applicable control-plane
profiles, not a separate cloud-authority model.

## Quick start

```bash
cargo build
cargo run --bin o3kd
```

For real libvirt execution use [docs/TESTLAB.md](docs/TESTLAB.md).

## Read the design

- [Architecture](docs/ARCHITECTURE.md)
- [Visual summary](docs/architecture/O3K_CLOUD_OS_SUMMARY.md)
- [ADR-0165 — Cloud OS / Cloud Kernel](docs/adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0166 — O3K IAM / Keystone compatibility](docs/adr/ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md)
- [ADR-0167 — Kubernetes-native control plane](docs/adr/ADR-0167-kubernetes-native-control-plane-deployment.md)
- [Product requirements](docs/PRODUCT_REQUIREMENTS.md)
- [Roadmap](docs/ROADMAP.md)
- [Normative source map](docs/NORMATIVE_SOURCES.md)

## Development model

O3K is a clean-slate Rust implementation owned and developed by Kubedo GmbH.
It is based on public OpenStack APIs/specifications, public client behavior, O3K
ADRs/specifications/contracts, and independently produced evidence.

## License

Apache-2.0.
