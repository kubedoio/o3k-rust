# SPEC-0025 — Rust rewrite and architecture convergence

Status: Accepted

Related decisions and specifications:

- [ADR-0002](../adr/ADR-0002-testlab-first.md)
- [ADR-0004](../adr/ADR-0004-contract-before-breadth.md)
- [ADR-0151](../adr/ADR-0151-public-go-o3k-reference-policy.md)
- [ADR-0160](../adr/ADR-0160-service-topology-and-execution-boundaries.md)
- [ADR-0162](../adr/ADR-0162-contract-first-staged-runner-validation.md)
- [ADR-0163](../adr/ADR-0163-product-profiles-and-deployment-posture.md)
- [SPEC-0021](SPEC-0021-cross-service-workflows-and-compensation.md)
- [SPEC-0022](SPEC-0022-service-api-baseline-and-evidence-gates.md)
- [SPEC-0024](SPEC-0024-product-profiles-and-claims.md)
- [Execution boundary contract](../../contracts/execution-boundaries.md)
- [Architecture boundary ratchet](../../contracts/core-architecture-boundaries.toml)

## Purpose

This specification defines how O3K Rust becomes the successor to the public Go
O3K implementation without turning the rewrite into a handler-for-handler or
package-for-package translation.

The rewrite must preserve useful OpenStack-facing behavior and operational
lessons while deliberately replacing architectural coupling. The target is not
"the Go implementation in Rust". The target is the smallest credible
OpenStack-compatible control plane whose supported workflows are specified,
recoverable, observable, and evidence-backed.

## Source and compatibility authority

The authority order remains:

1. official OpenStack API documentation and published specifications;
2. public OpenStack clients, SDKs, Terraform behavior, and Tempest where
   applicable;
3. accepted O3K Rust ADRs, specs, contracts, tests, and source-bound black-box
   evidence;
4. public Go O3K as a non-normative secondary reference.

Go O3K may contribute:

- route and field inventory;
- client-compatibility gaps;
- failure and cleanup scenarios;
- installer and operator requirements;
- real-world libvirt/network/storage lessons;
- regression scenarios to reproduce independently.

Go O3K must not define:

- Rust crate boundaries;
- persistence abstractions;
- lifecycle state representation;
- process topology;
- retry semantics where they conflict with the Rust operation model;
- OpenStack behavior that conflicts with the official/public compatibility
  authority.

Mechanical translation remains prohibited under the clean-implementation and
public-reference policies.

## Rewrite success criteria

The Rust rewrite is considered to be replacing a Go capability only when the
selected Rust product profile provides the required public outcome with its own
accepted architecture and evidence.

Replacement is measured by user workflows and declared compatibility records,
not by:

- repository size;
- route count;
- package/crate count;
- line count;
- matching Go database tables;
- matching Go internal types;
- implementing every endpoint that happens to exist in Go.

A Go route may remain absent indefinitely when no accepted Rust product profile
or user workflow requires it.

## Architecture convergence before breadth

Before broad API expansion, the native Rust TestLab must converge on the
following boundaries.

### 1. Canonical domain model

For O3K-owned resources, canonical durable identities, lifecycle states,
transitions, ownership rules, and operation semantics belong to the core domain.

Requirements:

- application services use typed resource IDs and typed lifecycle values where
  a domain concept exists;
- public OpenStack status strings are adapter projections;
- SQL/serialized state values are persistence projections;
- provider/agent states are observation projections;
- a new lifecycle rule is added once to the domain and tested there before
  adapters project it outward;
- duplicated free-form lifecycle semantics are migration debt and must not
  increase.

The first convergence priority is compute/server lifecycle because it crosses
Nova, Placement, provider dispatch, reconciliation, and libvirt evidence.

### 2. Repository ports before additional database support

SQLite remains the supported first database. The purpose of repository ports is
not to prematurely implement PostgreSQL; it is to stop SQLite details from
becoming application semantics.

Requirements:

- application services depend on narrow repository traits appropriate to their
  use cases;
- the composition root chooses the SQLite adapter;
- SQLx and `SqliteStore` are adapter details;
- store conformance is expressed against repository behavior, not SQL layout;
- PostgreSQL remains planned until a real adapter passes the same applicable
  conformance suite.

Direct `SqliteStore` use in application code was explicit migration debt.
Issue #510 (step 2 of the required implementation sequence below) removed the
existing exceptions behind the narrow `IdentityRepository`,
`KeypairRepository`, `VolumeAttachmentRepository`, and `ComputeRepository`
ports; the architecture-boundary ratchet continues to prevent new occurrences.

Issue #514 (step 3 of the required implementation sequence below) moved
Glance-compatible image metadata behind the narrow `ImageRepository` port on
the SQLite adapter: public image identity, project ownership, format,
visibility, size, checksum, and lifecycle state are durable-store
authoritative, restart reconstructs them from the store rather than from
directory or sidecar-file naming, and a missing or corrupt artifact for active
metadata fails closed. Uploaded bytes, the content-addressed cache, qcow2
verification, compute-host base images and overlays, and temporary publication
files remain the artifact authority in the bounded filesystem store; image
contents are not stored in SQLite.

Issue #516 (step 4 of the required implementation sequence below) moved
Neutron-compatible network/subnet/port control-plane metadata behind the
narrow `NetworkRepository` port on the SQLite adapter: network/subnet/port
identity and project ownership, subnet CIDR/gateway/allocation data,
deterministic MAC and fixed-IP allocation, port dependency, and
selected-host/binding intent with desired/observed binding state are
durable-store authoritative. SQLite UNIQUE constraints on `(subnet_id,
fixed_ip)` and `mac_address` make duplicate allocation impossible under the
supported single-`o3kd` concurrency, with allocation retry on unique
violation. The store layer is additionally safe for concurrent writers on
one SQLite file: independent service instances conflict deterministically
on duplicate names, CIDRs, and deletes, and never allocate a duplicate IP or
MAC (executable multi-writer tests), while deployment remains a single
`o3kd` process. The previous `metadata.json` file is imported once,
idempotently and crash-resume safely, then renamed so it is never read
again. Binding intent is recorded when a create dispatch selects a host, and
terminal create outcomes project `bound`/`error` onto the recorded intent
(live agent updates and reconcile paths, best-effort and idempotent), while
terminal delete success unbinds the ports so they are reusable. Host-local
TAP/bridge/DHCP execution and the ownership fences around foreign links
remain agent-owned and unchanged; the first-alpha flat-network public
behavior is unchanged.

Issue #523 (step 5 of the required implementation sequence below) moved
Placement provider inventory, generations, usage, and allocations behind the
narrow `PlacementRepository` port on the SQLite adapter (migration 0017):
provider state and generation, per-class inventories whose `used` values are
derived from the durable allocation rows rather than trusted reports,
allocations idempotent by the `allocation-{server_id}` key, and pending
allocation intents. Mutations execute in BEGIN IMMEDIATE transactions with
optimistic generation guards (`UPDATE ... WHERE generation = ?`), so
concurrent scheduler attempts cannot over-allocate: a losing attempt observes
a stale generation and the scheduler deterministically moves to the next
candidate (executable multi-writer tests over one SQLite file). A restart
reopens the same SQLite file and preserves provider generation, allocation,
and intent identity, so it cannot forget an allocation and schedule a
duplicate server. The previous `placement.json` and `allocation-intents.json`
journals are imported once, idempotently and crash-resume safely
(row-granular skip-if-present with the exact stored generation), then renamed
so they are never read again. Server create still persists the selected
provider/allocation identity before provider mutation (SPEC-0021 ordering
unchanged), and an unknown provider outcome retains the allocation until a
proven terminal outcome releases it exactly once. VCPU/MEMORY_MB/DISK_GB
semantics, generation-conflict behavior, allocation idempotency, and the
deterministic scheduler candidate order are unchanged; `GET /placement`
discovery remains version-only, and no routers, floating IPs, security
groups, VLAN/VXLAN, OVS, OVN, PostgreSQL, or edge/HA claims are added.

### 3. Durable control-plane metadata authority

The durable store becomes authoritative for O3K-owned public metadata and
recovery state, including:

- project/resource ownership;
- image metadata, checksum/size/format identity, and visibility;
- network, subnet, port, fixed-IP, MAC, and host-binding intent;
- Placement providers, inventory, generation, usage, and allocations;
- server desired/observed state, immutable dependency snapshots, and provider
  mappings;
- operation, compensation, and reconciliation state.

Files and backend resources remain appropriate for bounded bytes and host-local
execution artifacts such as image contents, qcow2 caches/overlays, config-drive,
console output, agent journals, and ownership manifests.

Moving metadata to the durable store must preserve existing artifact ownership
and checksum/path-safety rules. It is not a request to put image contents into
SQLite.

### 4. Public API adapters remain adapters

`o3k-api` may remain one crate for the first alpha, but it should be internally
organized by service/protocol concern so that Keystone-, Glance-, Neutron-,
Nova-, Placement-, and later Cinder-compatible request/response models do not
become one growing file.

Requirements:

- Axum routing/extractors and OpenStack JSON types stay outside the domain;
- service-specific request/response/error/microversion mapping is grouped by
  adapter concern;
- splitting the file must not change public behavior;
- a new workspace crate is not created merely to mirror an OpenStack service
  name.

### 5. Provider and external-service adapters stay outside application semantics

Agent protobufs, compute-agent connection state, libvirt types, CellHV models,
and external Cinder client models are adapter concerns.

Application use cases depend on bounded provider/external-service ports and
application-level result types. Existing places where agent protocol or Cinder
client types appear in application services are migration debt and must not be
used as templates for new work.

## First-alpha critical path

The release-blocking native TestLab workflow remains:

```text
discover/authenticate
-> upload and activate image
-> create network/subnet/port
-> create flavor/keypair
-> publish inventory and allocate Placement resources
-> create server
-> transfer verified artifacts to o3k-compute
-> realize flat networking and config-drive
-> create and boot libvirt/QEMU guest
-> observe ACTIVE according to the declared profile
-> prove console and guest config-drive consumption
-> stop/start/hard reboot
-> restart o3kd, o3k-compute, and supported libvirt dependency
-> reconcile the same server identity
-> delete
-> prove all O3K-owned resources absent and foreign state unchanged
```

Work is release-critical when it closes a gap in this workflow, its failure
matrix, its installation/cleanup path, or its required evidence.

Native Cinder, boot-from-volume, PostgreSQL, HA, broad Neutron, metadata HTTP,
external Keystone, additional service-testbed profiles, and edge multi-host
features do not block this alpha unless a later accepted human decision changes
the release gate.

## External Cinder priority rule

The external-Cinder service-testbed remains a valid product profile and may
progress in parallel.

It is non-blocking for the first native alpha. Therefore:

- external-Cinder fixes may merge when bounded and independently valuable;
- shared identity/attachment/security fixes may benefit both profiles;
- protected Cinder/Tempest debugging must not replace missing native TestLab
  architecture or full guest evidence;
- a new external-Cinder requirement must not expand the native alpha baseline
  unless the native workflow independently needs it;
- roadmap or release language must not make external-Cinder evidence a hidden
  prerequisite for the first native alpha.

## Compatibility expansion after the first vertical slice

After the native workflow is stable, compatibility expands by client/user
journey.

Preferred discovery order:

1. python-openstackclient workflows;
2. openstacksdk workflows;
3. Terraform OpenStack provider workflows;
4. Horizon workflows where selected;
5. focused Tempest subsets tied to declared operations;
6. additional Go O3K behavior inventory that corresponds to an accepted user
   outcome.

For each missing capability:

```text
user/client failure
-> classify exact OpenStack operation/field/microversion
-> verify official/public behavior
-> add or amend compatibility record
-> add black-box contract test
-> implement through the existing domain/application boundary
-> update evidence
```

Do not add adjacent endpoints because they are easy or because they increase a
coverage percentage.

## Operational replacement after functional replacement

The Go implementation contains valuable product/operations behavior. Once the
native TestLab control-plane lifecycle is reliable, Rust should reproduce the
selected user outcomes independently, including where required:

- zero-config startup;
- one-line/package installation;
- clean systemd lifecycle;
- health/readiness;
- metrics and tracing;
- backup/restore and upgrade guidance;
- signed release artifacts, checksums, SBOM, and provenance;
- safe reset/uninstall/purge;
- operator diagnostics and bounded evidence.

Operational parity is selected by product requirements and evidence, not source
translation.

## Architecture fitness ratchet

`contracts/core-architecture-boundaries.toml` records machine-checkable
boundaries and temporary known debt.

The ratchet activates in normal CI when the contract status is `accepted`.
While the contract is not yet accepted (e.g. `proposed`, awaiting the human
decision PR), the checker runs in deferred mode: it still validates the
contract structure and the exhaustive crate classification, but it does not
reject changes based on boundary rules that have not been accepted. Once the
decision is accepted, normal CI must fail when:

- the domain gains a forbidden outward dependency;
- a new application crate depends on protocol/database/execution adapters that
  are prohibited by the contract;
- a new direct `SqliteStore` occurrence appears outside the temporary explicit
  debt list;
- the boundary contract is malformed or names a missing crate/file.

The ratchet is intentionally asymmetric:

- removing debt is always allowed;
- moving an exception to a smaller scope is allowed;
- adding or broadening an exception requires architecture review and an
  explicit follow-up rationale.

The ratchet does not claim current architecture debt is already fixed. Its first
purpose is to stop the debt from spreading while the next issues remove it.

## Required implementation sequence

Unless a release-blocking defect requires a smaller emergency fix, the next
foundation work should proceed in this order:

1. canonicalize compute/server domain identities and lifecycle states;
2. introduce narrow compute/identity repository ports and remove direct
   `SqliteStore` application dependencies;
3. move image metadata to durable store authority while keeping content in the
   bounded artifact store;
4. move network/subnet/port allocation and binding metadata to durable store
   authority;
5. move Placement inventory/allocation persistence behind a repository port;
6. separate agent/Cinder adapter types from compute/reconciler application
   semantics;
7. split `o3k-api` internally by protocol/service concern without behavior
   change;
8. run the complete native TestLab vertical slice and failure/restart matrix;
9. expand compatibility from real client failures, not route inventory alone;
10. recover selected operational product behavior from Go as independent Rust
    requirements.

Steps may be divided into coherent issues and PRs. Do not combine all
persistence migrations into one big-bang rewrite.

## Required tests

### Architecture

- machine-readable dependency-boundary validation in normal CI;
- no new concrete-store exception;
- no new outward domain dependency;
- repository-port tests use stateful in-memory or SQLite adapters as
  appropriate without duplicating implementation expectations.

### Domain

- canonical server-state transition matrix;
- projection tests from provider observations into canonical state;
- projection tests from canonical state into Nova-compatible status;
- invalid or ambiguous state conversion fails closed.

### Persistence

- repository conformance for every extracted port;
- migration/restart preserves durable IDs and operations;
- SQLite WAL/concurrency behavior remains intact;
- metadata/blob separation survives backup/restore and restart;
- no host-local artifact path is required to reconstruct public identity.

### Compatibility

- current TestLab baseline continues to pass after each refactor;
- black-box HTTP/client behavior is unchanged by architecture-only changes;
- unsupported operations remain unadvertised;
- a Go behavior item is not marked replaced without corresponding Rust public
  evidence.

### Real host

- the first-alpha workflow proves the same server identity across supported
  restarts;
- no duplicate domain, port, allocation, or artifact is created after unknown
  outcomes;
- cleanup removes only O3K-owned resources;
- foreign state remains unchanged.

## Non-goals

- immediate full Go route parity;
- immediate PostgreSQL implementation;
- moving image contents or guest disks into SQLite;
- one giant repository trait for every resource;
- one crate per OpenStack service;
- one daemon per logical service;
- native Cinder before the ephemeral-root guest;
- rewriting stable execution-safety code merely to change style;
- accepting a new architecture claim without executable evidence where a
  fitness check is practical.
