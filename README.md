# O3K

<p align="center">
  <strong>One Cloud Operating System from edge to datacenter.</strong><br />
  Start with a few prepared hosts. Scale by adding capability-bearing building blocks without replatforming.<br />
  OpenStack-compatible northbound. O3K-native cloud authority in the middle. Provider-neutral typed execution southbound.
</p>

<p align="center">
  <img alt="Status: alpha" src="https://img.shields.io/badge/status-alpha-f59e0b" />
  <img alt="Implementation: Rust" src="https://img.shields.io/badge/implementation-Rust-111827" />
  <img alt="Kubernetes target" src="https://img.shields.io/badge/Kubernetes-first--class%20target-326ce5" />
  <img alt="License: Apache 2.0" src="https://img.shields.io/badge/license-Apache--2.0-3b82f6" />
</p>

![O3K Cloud Operating System architecture](docs/architecture/o3k-cloud-os.svg)

O3K is a **cloud kernel — literally born in the cloud**. It is built from scratch in Rust around a shared cloud authority rather than inherited service boundaries. The Cloud Kernel owns identity and authorization, resource ownership, desired state, operations, scheduling, reconciliation, quotas, audit/event identity, and failure semantics; compatibility APIs stay northbound and infrastructure execution stays southbound.

The product goal is **one operating system end to end**: from a small office or
factory server room, through a customer-owned datacenter cage, to a larger
private or datacenter cloud. The same O3K resource model, IAM, APIs, service
catalog, automation and execution contracts remain in place as the deployment
grows. Larger profiles may introduce cells, sharding, hierarchical scheduling,
or different provider topologies internally; customers should not have to move
from an "edge product" to a different "datacenter product" simply because they
added capacity.

O3K is **not** a service-for-service Rust rewrite of Nova, Neutron, Keystone,
Glance, Placement, and Cinder. OpenStack service names define compatibility
surfaces; O3K owns its internal cloud model.

Core principles:

- **One Cloud OS from edge to datacenter.** Edge and datacenter are scale/evidence
  profiles of the same product architecture, not different O3K products.
- **Scale by building blocks, not by replatforming.** Capacity, capabilities and
  failure domains are added behind the same cloud contracts.
- **OpenStack compatibility is northbound.** Existing CLI/SDK/Terraform
  workflows remain valuable contracts and a path into the wider OpenStack
  service ecosystem.
- **O3K owns cloud authority.** Public IDs, ownership, desired state,
  scheduling, operations, and reconciliation are O3K concerns.
- **The Cloud Kernel is shared.** IAM, authorization, resource ownership,
  operations, quotas, audit/event identity, and failure semantics are reused by
  first-class O3K domains.
- **The service catalog is composable.** A deployment can expose only the cloud
  capabilities it needs and may combine native O3K services with explicitly
  supported external-hosted services.
- **Execution is southbound.** Host agents/providers perform bounded mutations
  and report observations.
- **Kubernetes is a first-class deployment target.** Kubernetes may operate the
  O3K control plane, but it does not become O3K's VM scheduler, tenant-resource
  database, or Cloud Kernel.

> **Current status:** alpha. P9 (routed fabric), P10 (native persistent
> storage — LVM + Ceph RBD), and P11 (multi-hypervisor edge cloud with
> overlapping tenant CIDRs, Geneve+WireGuard, three-host real evidence)
> are completed. The small-edge-cloud profile (10–20 hypervisors target)
> has real-host evidence. It is the first proven scale rung, not evidence for
> arbitrary datacenter scale. Production HA, full OpenStack parity, and broad
> maximum-scale are not current support claims.

## One Cloud OS from edge to datacenter

The intended scale continuum is:

```text
office / branch / factory
        -> customer server room
        -> dedicated datacenter cage
        -> multi-rack private cloud
        -> datacenter-scale cloud
```

The invariant across that continuum is the product contract, not a frozen
internal process topology:

```text
same IAM / AuthContext
same resource ownership model
same O3K native API family
same selected OpenStack compatibility contracts
same Operation / reconciliation semantics
same service catalog model
same Terraform / OpenTofu approach
same Araf product model
same typed execution-provider contracts
```

A small deployment may be operationally simple. A larger deployment may need
HA controllers, PostgreSQL topologies, cells, sharding, hierarchical capacity,
multiple fabric/storage domains, or other scale mechanisms. Those mechanisms
must remain behind the same O3K cloud semantics so growth does not become a
customer replatforming event.

O3K calls the capacity/capability unit in this model a **deployment building
block**. A block may contribute compute, network, storage, control-plane
participation, or other capabilities and carries explicit failure-domain
identity. It is not a fixed hardware SKU or a requirement that every service
runs on every node.

See [ADR-0181 — edge-to-datacenter building-block Cloud OS](docs/adr/ADR-0181-edge-to-datacenter-building-block-cloud-os.md)
and [SPEC-0038 — edge-to-datacenter building-block cloud](docs/specs/SPEC-0038-edge-to-datacenter-building-block-cloud.md).

## One-line TestLab install (alpha)

On a clean Ubuntu 24.04 or Debian 12 x86_64 VM:

```bash
curl -sfL https://get.o3k.io | sudo sh -
```

`get.o3k.io` is only a convenience redirect to the official GitHub Release
asset. The canonical direct alpha URL is:

```bash
curl -sfL https://github.com/o3kio/o3k/releases/download/v0.2.0-alpha.2/install.sh | sudo sh -
```

The future stable URL will be
`https://github.com/o3kio/o3k/releases/latest/download/install.sh` —
it is **not** the alpha source and must not be used before a stable release
exists.

The installer is pinned to its own release: it installs the verified
`v0.2.0-alpha.2` release bundle, bootstraps the libvirt TestLab (`test-vm`
ACTIVE, console verified), and writes client credentials to
`/etc/o3k/admin-openrc` and `/etc/o3k/clouds.yaml` — the admin password is
never printed.

**Supported:** Ubuntu 24.04 x86_64, Debian 12 x86_64, libvirt TestLab alpha.
Version pinning and the dev/test overrides (`O3K_VERSION`,
`O3K_RELEASE_BASE`), credentials, idempotent re-run, uninstall/purge, and
troubleshooting: [docs/INSTALLER.md](docs/INSTALLER.md).

**Not claimed:** production, HA, Kubernetes HA, PostgreSQL, full OpenStack,
native Cinder, ARM/RHEL/etc.

Fast bootstrap on prepared infrastructure is a product target. Claims such as
"in seconds" or "in minutes" remain profile-specific measurement claims and
must not include pre-existing physical, storage, network or external-service
provisioning as if O3K performed it.

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
 o3k-compute  o3k-network  o3k-storage
      |            |            |
libvirt     Geneve+WG     LVM / Ceph RBD
```

`o3kd` is the current integrated control-plane composition shell. Host-local
real compute, network, and storage execution cross typed gRPC+mTLS agent
boundaries. Multi-host topology with overlapping tenant CIDRs, Geneve realm
encapsulation over WireGuard host transport, and LVM/RBD storage is proven
on three-host real evidence with 15 simulated scale hosts.

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

## Composable service catalog

OpenStack compatibility is also an ecosystem extension boundary. O3K does not
need to reimplement every specialized OpenStack project in Rust to make that
capability available to an O3K deployment.

An operator-selected catalog may conceptually look like:

```text
Identity        native O3K
Image           native O3K
Compute         native O3K
Network         native O3K
Volume          native O3K
Load Balancing  external-hosted Octavia   (when profile-proven)
DNS             external-hosted Designate (when profile-proven)
Secrets         external-hosted Barbican  (when profile-proven)
```

Other deployments may expose only the minimal core or select different
capabilities for AI/GPU edge, sovereign cloud, storage-heavy, or datacenter
profiles.

External OpenStack services are **not** assumed to work automatically. Each
hosted-service profile must freeze the exact upstream version and discover/prove
the exact O3K/OpenStack dependency behavior it consumes before the catalog can
advertise support. Catalog registration never converts an external-hosted
service into an O3K-native implementation claim.

## Persistence

- **SQLite**: supported default for TestLab and single-controller profiles.
- **PostgreSQL**: supported production-oriented persistence profile (verified with PostgreSQL 16).

PostgreSQL is a prerequisite for the future HA Kubernetes profile. O3K does not
use shared-SQLite or distributed-filesystem workarounds as a shortcut to Kubernetes HA.

## Product profiles

O3K has one product architecture. Deployment/evidence profiles prove bounded
parts of the same edge-to-datacenter continuum and must not be mistaken for
separate O3K products:

- **OpenStack service testbed** — host selected external OpenStack services
  against declared O3K compatibility surfaces;
- **native O3K TestLab/cloud** — minimal/single-host evidence for the native
  Cloud Kernel and IaaS path;
- **small edge cloud** — the first real multi-host scale rung: P11 proves
  overlapping CIDRs, Geneve+WireGuard fabric, LVM locality, serial RBD,
  drain/restart/failure recovery, three real hosts and 15 simulated scale hosts,
  with an initial target around 10–20 hypervisors;
- **future larger private/datacenter profiles** — must use the same O3K cloud
  contracts while publishing their own exact host counts, HA/database/provider
  topology, performance budgets and failure evidence.

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
- [ADR-0181 — edge-to-datacenter building-block Cloud OS](docs/adr/ADR-0181-edge-to-datacenter-building-block-cloud-os.md)
- [SPEC-0038 — edge-to-datacenter building-block cloud](docs/specs/SPEC-0038-edge-to-datacenter-building-block-cloud.md)
- [Product requirements](docs/PRODUCT_REQUIREMENTS.md)
- [Roadmap](docs/ROADMAP.md)
- [Normative source map](docs/NORMATIVE_SOURCES.md)

## Development model

O3K is a clean-slate Rust implementation owned and developed by Kubedo GmbH.
It is based on public OpenStack APIs/specifications, public client behavior, O3K
ADRs/specifications/contracts, and independently produced evidence.

## License

Apache-2.0.
