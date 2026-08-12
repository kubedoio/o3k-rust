# O3K

<p align="center">
  <strong>Rust-native Cloud Operating System with an OpenStack-compatible northbound surface.</strong><br />
  Keep the useful OpenStack API ecosystem. Replace the historical service topology with a shared cloud model, durable control loops, and typed infrastructure execution.
</p>

<p align="center">
  <img alt="Status: alpha" src="https://img.shields.io/badge/status-alpha-f59e0b" />
  <img alt="Implementation: Rust" src="https://img.shields.io/badge/implementation-Rust-111827" />
  <img alt="License: Apache 2.0" src="https://img.shields.io/badge/license-Apache--2.0-3b82f6" />
</p>

![O3K Cloud Operating System architecture](docs/architecture/o3k-cloud-os.svg)

O3K is **not** an attempt to rebuild Nova, Neutron, Glance, Keystone, Placement,
and Cinder as the same collection of internal services in Rust.

Those names matter at the compatibility boundary. Inside O3K, the architecture
is deliberately different:

- **OpenStack compatibility is northbound.** Existing clients and selected
  OpenStack workflows remain first-class contracts.
- **O3K owns the canonical cloud model.** Identity, ownership, desired state,
  operations, scheduling, and reconciliation are O3K concerns.
- **Cloud Kernel primitives are shared.** New O3K domains should reuse common
  authorization, resource, operation, quota, audit, and failure semantics.
- **Infrastructure mutation is southbound.** Typed provider contracts isolate
  cloud semantics from libvirt/KVM and future execution implementations.
- **Architecture and current runtime are stated separately.** The Cloud Kernel
  is the product architecture; today it is largely composed inside `o3kd`, not
  deployed as a fleet of invented microservices.

> **Current status:** alpha. The `v0.2.0-alpha.1` release direction is a
> Rust-native, OpenStack-compatible libvirt TestLab—not a claim of production
> HA, full OpenStack parity, PostgreSQL support, native persistent volumes, or a
> fixed memory footprint.

## What actually runs today

The current runtime is intentionally simpler than the long-term architecture.
`o3kd` is the integrated control-plane process. A separate `o3k-compute` process
is used when host-local execution must cross a machine/process boundary.

![O3K current runtime topology](docs/architecture/o3k-runtime-topology.svg)

| Responsibility | Current implementation |
|---|---|
| Control-plane composition | `bins/o3kd` |
| OpenStack/API adapters | `crates/o3k-api` |
| Identity and authorization context | `crates/o3k-identity` |
| Canonical resource state/invariants | `crates/o3k-domain` |
| Durable state and migrations | `crates/o3k-store` — SQLite is the supported minimal default |
| Image domain | `crates/o3k-image` |
| Network domain | `crates/o3k-network` |
| Capacity / Placement | `crates/o3k-placement`, `crates/o3k-scheduler` |
| Compute orchestration | `crates/o3k-compute` |
| Reconciliation | `crates/o3k-reconciler` |
| Execution abstraction | `crates/o3k-provider`, `crates/o3k-provider-contract` |
| Direct libvirt execution | `crates/o3k-libvirt` |
| Remote host execution | `bins/o3k-compute`, `crates/o3k-compute-agent`, gRPC + mTLS |
| Cinder compatibility/testbed integration | `crates/o3k-cinder`; external Cinder retains its own service authority |

`o3kd` should therefore be read as a **composition shell**, not as an intended
future monolith and not as evidence that every conceptual Cloud Kernel boundary
needs its own daemon.

## The control loop is the architecture

The most important O3K boundary is not an HTTP route or a process name. It is
the authority flow from user intent to durable state to bounded infrastructure
execution and back to observed state.

![O3K durable control loop](docs/architecture/o3k-control-loop.svg)

For O3K-owned resources, the control plane owns public identity, tenant/security
scope, desired state, scheduling decisions, durable operation identity, and
reconciliation. Providers own **bounded mutation and observation**.

That leads to a core distributed-systems rule:

```text
request timeout != proven failure
```

A lost response is an unknown outcome. O3K records the operation, observes the
provider, reconciles identity, and only retries when the execution contract
makes that safe. Compensation is explicit rather than accidental.

## OpenStack compatibility without OpenStack internal topology

OpenStack service names describe public compatibility surfaces. They do not
force O3K to copy historical service/process boundaries internally.

| OpenStack surface | O3K domain |
|---|---|
| Keystone | O3K IAM / identity compatibility |
| Glance | O3K Image |
| Nova | O3K Compute |
| Neutron | O3K Network |
| Placement | O3K Capacity / Placement |
| Cinder | O3K Volume compatibility / hosted-service integration today; native volume is later |

The compatibility model is therefore:

```text
OpenStack client
      |
      v
compatibility adapter
      |
      v
O3K IAM + domain model + durable operation semantics
      |
      v
typed execution contract
      |
      v
infrastructure provider
```

OpenStack **2026.1 Gazpacho** is the primary external compatibility reference.
OpenStack **2025.2 Flamingo** is a backward reference where a compatibility
window is explicitly declared. O3K advertises only behavior that is implemented
and evidenced.

See:

- [OpenStack target manifest](compatibility/openstack-targets.yaml)
- [product profile manifest](compatibility/product-profiles.yaml)
- [SPEC-0022 — service API baseline and evidence gates](docs/specs/SPEC-0022-service-api-baseline-and-evidence-gates.md)

## Cloud Kernel: shared semantics, not a dumping ground

The Cloud Kernel is the set of cross-domain primitives that should not be
reimplemented every time O3K gains a new service capability:

- principals, service identity, and a typed `AuthContext`;
- authorization and tenant/resource ownership;
- durable public IDs versus provider IDs;
- service registry and compatibility projections;
- quotas and limits;
- durable operations and idempotency identity;
- regions / availability-zone identity;
- audit and event identity;
- failure and unknown-outcome semantics;
- compensation and reconciliation.

Protected operations converge on the conceptual authorization contract:

```text
Principal × Action × Resource × Context -> Allow | Deny
```

Service-specific business logic stays in its domain. The Kernel owns shared
cloud semantics; it does not become a bucket for every feature.

## Product profiles

O3K has one product architecture with three primary deployment/evidence
profiles.

### OpenStack service testbed

Run selected surrounding OpenStack-compatible APIs required to develop or test
an independently operated OpenStack service without standing up a complete
DevStack/full control plane. External services such as hosted Cinder keep their
own database, message bus, backend, migrations, upgrades, and operational
authority.

### Native O3K TestLab / cloud

O3K owns the selected IAM, image, network, capacity, compute, durable state, and
provider mappings. The first real-cloud milestone is an ephemeral-root libvirt
lifecycle:

```text
authenticate
-> upload image
-> create network/subnet/port
-> create flavor/keypair
-> allocate compute resources
-> boot QEMU/KVM guest
-> inspect lifecycle
-> restart and reconcile
-> delete and prove cleanup
```

Native persistent volumes are a later milestone.

### Small edge cloud

The architecture is intended to grow into a lightweight multi-host cloud for an
initial target of roughly 10–20 hypervisors. Process boundaries are introduced
only after the logical authority/provider boundary is stable and evidence shows
that another daemon is justified.

See [SPEC-0024 — product profiles and claims](docs/specs/SPEC-0024-product-profiles-and-claims.md).

## Quick start

Prerequisite: the Rust toolchain pinned in
[`rust-toolchain.toml`](rust-toolchain.toml).

```bash
cargo build
cargo run --bin o3kd
```

`o3kd` starts with safe development defaults: fake provider, API on
`127.0.0.1:8080`, and data under `./data`.

Token authentication remains disabled until `O3K_BOOTSTRAP_PASSWORD` and
`O3K_TOKEN_SIGNING_KEY` are configured. Generate protected values with:

```bash
scripts/generate-passwords.sh
```

For a real libvirt workflow, follow the [TestLab guide](docs/TESTLAB.md) rather
than treating the development defaults as a production deployment.

## Repository map

```text
bins/
  o3kd/                    integrated control-plane composition
  o3k-compute/             host-local compute executor

crates/
  o3k-api/                 HTTP + OpenStack compatibility adapters
  o3k-identity/            identity / auth context foundations
  o3k-domain/              canonical resource states and invariants
  o3k-store/               durable state and migrations
  o3k-image/               image domain
  o3k-network/             network domain
  o3k-placement/           inventory / allocations / placement
  o3k-scheduler/           scheduling decisions
  o3k-compute/             compute orchestration
  o3k-reconciler/          desired/observed convergence
  o3k-provider/            provider abstraction
  o3k-provider-contract/   typed execution contracts
  o3k-compute-agent/       remote execution implementation
  o3k-libvirt/             libvirt/KVM provider
  o3k-cinder/              Cinder compatibility/testbed integration

docs/                      architecture, ADRs, specs, operations
compatibility/             declared OpenStack/product profiles
contracts/                 architecture/public/execution contracts
proto/                     versioned execution protocols
```

The workspace grows when a real authority boundary or evidence requirement
justifies it—not because another historical OpenStack project exists.

## Read the design

Start here if you are reviewing or extending O3K:

1. [Architecture](docs/ARCHITECTURE.md)
2. [Visual architecture summary](docs/architecture/O3K_CLOUD_OS_SUMMARY.md)
3. [ADR-0165 — O3K Cloud Operating System and Cloud Kernel](docs/adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
4. [ADR-0166 — O3K IAM and Keystone compatibility boundary](docs/adr/ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md)
5. [SPEC-0020 — Keystone trust, catalog, and auth context](docs/specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md)
6. [SPEC-0024 — product profiles and claims](docs/specs/SPEC-0024-product-profiles-and-claims.md)
7. [Normative source map](docs/NORMATIVE_SOURCES.md)

For development policy, also read [AGENTS.md](AGENTS.md) and
[LLM development](docs/LLM_DEVELOPMENT.md).

## What O3K deliberately does not claim yet

The architecture is ahead of the release evidence in several areas. This is
intentional and documented rather than hidden.

O3K does **not** currently claim:

- production-ready HA control-plane operation;
- complete OpenStack API parity;
- PostgreSQL support;
- a native persistent-volume service;
- separate native network/storage daemons;
- broad federation to already-authoritative external clouds;
- a universal ~50 MB runtime guarantee.

SQLite is the supported minimal/TestLab default today. PostgreSQL is an intended
production-oriented profile only after a real adapter, migrations, transaction
semantics, backup/restore, and failure evidence exist.

## Development model and provenance

O3K is a clean-slate Rust implementation owned and developed by Kubedo GmbH. It
is based on public OpenStack APIs/specifications, public client behavior, O3K
ADRs/specifications/contracts, and independently produced black-box evidence.

The public Apache-2.0 Go O3K repository may be used only as a non-normative
secondary source for requirements discovery, route inventory, operational
lessons, and behavioral comparison. Mechanical translation is prohibited unless
separately approved and attributed.

O3K is developed heavily with LLM coding agents under human architecture,
security, and release review. Agents are expected to preserve authority
boundaries, desired/observed-state semantics, compatibility honesty, and
provenance.

## License

Apache-2.0. New dependencies and reused artifacts must satisfy the project's
license and provenance policy. The O3K name and Kubedo marks are not granted by
the Apache-2.0 software license.
