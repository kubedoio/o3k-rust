# O3K Rust

O3K is a lightweight, Rust-native OpenStack-compatible control plane for
reproducible OpenStack service testbeds, progressively native Rust cloud
services, and small edge clouds.

The project is owned and developed by Kubedo GmbH and licensed under
Apache-2.0. It is a clean-slate Rust implementation based on public OpenStack
APIs, public standards, public client behavior, project ADRs/specifications,
and independently produced black-box evidence. It is not a source-code port of
another O3K implementation.

> **Status:** pre-alpha. No production-readiness, full OpenStack parity,
> PostgreSQL support, HA, or fixed footprint claim is made yet.

## Product scope

O3K has three related product profiles.

### 1. OpenStack service testbed

O3K can provide the surrounding OpenStack-compatible services required to run
and test a selected real OpenStack service without deploying a complete
DevStack or full OpenStack control plane.

For example, a real external Cinder deployment can use O3K for the declared:

- Keystone-compatible service project, service user, roles, token issuance,
  public token validation, regions, endpoints, and service catalog;
- Glance-compatible image access required by the selected workflow;
- Nova-compatible volume-attachment surface and compute integration;
- optional Neutron- or Placement-compatible satellite behavior.

The external service remains independent. A hosted Cinder deployment still
requires its own supported database, message bus, Cinder processes, storage
backend, migrations, upgrades, and operational ownership. Registering an
external `volumev3` endpoint does not mean O3K implements Cinder.

See [SPEC-0023](docs/specs/SPEC-0023-external-cinder-service-under-test.md).

### 2. Native Rust OpenStack-compatible cloud

O3K progressively provides its own Rust-native compatibility profiles for:

- Keystone-compatible identity;
- Glance-compatible image services;
- Nova-compatible compute;
- Neutron-compatible networking;
- Placement-compatible capacity and allocations;
- Cinder-compatible volumes and attachments.

Compatibility is declared per service, operation, field, extension, and
microversion. O3K does not claim complete parity with a named OpenStack release
because selected routes exist.

The first native real-cloud milestone is an ephemeral-root libvirt TestLab:

```text
authenticate
-> upload image
-> create network/subnet/port
-> create flavor/keypair
-> allocate compute resources
-> create and boot a QEMU/KVM guest
-> inspect console and lifecycle
-> restart and reconcile
-> delete and prove cleanup
```

Native persistent volumes and `o3k-storage` are later milestones and do not
block the first guest.

### 3. Small edge cloud

O3K is designed to grow into a lightweight control plane for approximately
10–20 hypervisors.

The target execution topology is:

```text
                         o3kd
                           |
             +-------------+-------------+
             |             |             |
       o3k-compute    future o3k-network  future o3k-storage
       libvirt/KVM    host networking     LVM/Ceph and volumes
```

Logical `ComputeProvider`, `NetworkProvider`, and `StorageProvider` contracts
must be stable before new daemons are introduced.

An edge profile may integrate selected external OpenStack services, but
“connect to another OpenStack” is not one feature. External Keystone, endpoint
registration, hosted services, external Glance/Cinder/Neutron consumption,
federation, and cross-cloud resource sharing require separate contracts,
security decisions, and evidence.

See [ADR-0163](docs/adr/ADR-0163-product-profiles-and-deployment-posture.md) and
[SPEC-0024](docs/specs/SPEC-0024-product-profiles-and-claims.md).

## Why O3K exists

Traditional OpenStack deployments are powerful, but a complete deployment can
be too complex and expensive for service integration tests, developer
workflows, edge sites, training labs, and smaller operators.

O3K aims to:

- enable only the service profiles required by a scenario;
- replace a full DevStack control plane in selected service-under-test
  workflows;
- install quickly on one node and grow deliberately to a small multi-host
  environment;
- preserve useful, tested OpenStack API behavior;
- keep public API semantics separate from privileged host execution;
- recover through durable desired state, observations, and reconciliation;
- use standard QEMU/KVM through libvirt as the primary compute backend;
- integrate naturally with CellHV later through typed provider contracts;
- remain understandable and auditable for human and LLM-driven development.

## Architectural model

`o3kd` begins as a modular control-plane process containing logically separate
identity, image, compute, network, volume, and placement modules.

```text
OpenStack CLI / SDK / Terraform / external OpenStack service
                            |
                          o3kd
                            |
 identity | image | compute | network | volume | placement
                            |
       policy | scheduling | operations | reconciliation
                            |
             typed execution/provider contracts
                 /              |              \
          o3k-compute     future network    future storage
          libvirt/KVM      execution          execution
```

Keystone-compatible identity is the trust and service-discovery root. It is not
the transaction coordinator for servers, ports, volumes, or allocations.

O3K owns OpenStack-facing IDs, authorization, desired state, scheduling,
operation journals, compensation, reconciliation, compatibility behavior, and
mappings to provider-native resources. Execution agents own only bounded host
mutations and provider observations.

See [Architecture](docs/ARCHITECTURE.md),
[ADR-0160](docs/adr/ADR-0160-service-topology-and-execution-boundaries.md), and
[execution-boundary contracts](contracts/execution-boundaries.md).

## Database posture

### SQLite

SQLite is the currently supported default for the minimal TestLab and portable
simulated-cloud profiles.

Supported SQLite operation requires explicit WAL/concurrency behavior,
bounded lock handling, migrations, crash/restart testing, backup/restore
instructions, and documented filesystem constraints.

### PostgreSQL

PostgreSQL is the intended database for production-oriented, stronger
availability, and possible multi-controller profiles.

PostgreSQL must not be presented as currently supported or installable merely
because it is the architectural target. A supported claim requires a real
adapter, store-conformance suite, migrations, transaction semantics,
backup/restore behavior, and process/failure evidence.

Until those gates pass, the honest statement is:

> SQLite is the supported default. PostgreSQL is the planned
> production-oriented database profile.

## Resource-footprint target

The minimal O3K control plane targets an approximately **50 MB steady-state
memory footprint**. This is a target, not an unconditional guarantee.

Every published footprint number must identify:

- the exact product profile;
- included O3K processes;
- source commit, build mode, and features;
- host and measurement method;
- idle or workload phase;
- external dependencies reported separately.

External Cinder, RabbitMQ, PostgreSQL, libvirt, QEMU guests, Ceph, LVM, and
other hosted services are not hidden inside an O3K-only footprint number.

## Compatibility target

Official OpenStack API documentation and published specifications are
normative. Public OpenStack clients, SDKs, Terraform behavior, and Tempest are
the next references. O3K ADRs, contracts, and black-box evidence record
project-specific decisions.

The primary reference release is OpenStack **2026.1 Gazpacho**. OpenStack
**2025.2 Flamingo** is maintained as a backward-reference profile where
declared. O3K advertises only the contiguous API/microversion windows that are
implemented and verified.

See:

- [OpenStack target manifest](compatibility/openstack-targets.yaml);
- [product profile manifest](compatibility/product-profiles.yaml);
- [SPEC-0022](docs/specs/SPEC-0022-service-api-baseline-and-evidence-gates.md).

The public Apache-2.0 [Go O3K repository](https://github.com/kubedoio/o3k) may
be used only as a non-normative secondary reference for requirements discovery,
route inventory, operational lessons, and behavioral comparison. Mechanical
translation is prohibited unless separately approved and attributed.

## Development evidence ladder

Development proceeds from cheap, deterministic evidence toward privileged
integration:

```text
ADR / SPEC / contract
-> domain, store, migration, and policy tests
-> stateful provider conformance
-> portable simulated cloud
-> process-level client tests
-> compute/network/storage component gates
-> full-profile real-host gate
-> failure/restart matrix
-> release gate
```

The protected full-cloud runner is an integration verifier, not the primary
mechanism for discovering missing API requirements.

## Repository layout

```text
bins/o3kd/                 O3K control-plane binary
bins/o3k-compute/          compute execution agent
crates/o3k-api/            HTTP and OpenStack protocol adapters
crates/o3k-domain/         resource identities, states, and invariants
crates/o3k-store/          durable state and migrations
crates/o3k-network/        network domain/provider foundations
docs/                      architecture, ADRs, specs, product documents
contracts/                 public and execution-boundary contracts
compatibility/             OpenStack targets and product profiles
proto/provider/v1/         versioned execution protocols
.github/                   CI and contribution workflows
```

The workspace grows only when a specification and accepted issue justify a new
boundary.

## LLM-first development

O3K is developed primarily with LLM coding agents under human architectural,
security, and release review. LLM-first does not mean specification-free,
review-free, or evidence-free.

Every agent must:

1. read `AGENTS.md` and the relevant normative sources;
2. identify the product profile and evidence tier being changed;
3. work from an issue with explicit acceptance criteria;
4. add the closest useful tests before claiming completion;
5. preserve compatibility, security, ownership, and failure semantics;
6. report uncertainty instead of inventing OpenStack behavior;
7. keep public and non-public provenance boundaries intact;
8. update compatibility and evidence records when behavior changes.

See [AGENTS.md](AGENTS.md), [LLM development](docs/LLM_DEVELOPMENT.md), and
[clean implementation rules](docs/CLEAN_IMPLEMENTATION.md).

## Current release direction

The first public alpha remains the native ephemeral-root TestLab profile. It
must prove a complete OpenStack CLI lifecycle through O3K-owned identity,
image, network, placement, compute, `o3k-compute`, and libvirt/QEMU.

The external-Cinder service-testbed profile may progress in parallel but does
not replace or block that first alpha unless a later accepted release decision
changes the gate.

## Licensing

O3K Rust is licensed under Apache-2.0. New dependencies and reused artifacts
must pass the project license and provenance policy. The O3K name and Kubedo
marks are not granted by the Apache-2.0 software license.
