# O3K Rust — ordered implementation prompts

These prompts are intended for coding agents after the architecture correction
in ADR-0160 and SPEC-0025 is reviewed. Execute them in order unless a
release-blocking defect requires a smaller emergency fix.

Each prompt is deliberately bounded. Do not merge adjacent prompts merely to
save agent turns. The goal is to remove architecture debt without destabilizing
the already-working TestLab behavior.

## Common preamble for every prompt

Use this preamble before the task-specific text:

```text
Repository: kubedoio/o3k-rust
Product profile: native-rust-testlab unless explicitly stated otherwise.

Before editing, read AGENTS.md, docs/NORMATIVE_SOURCES.md,
docs/CLEAN_IMPLEMENTATION.md, docs/adr/ADR-0002-testlab-first.md,
docs/adr/ADR-0004-contract-before-breadth.md,
docs/adr/ADR-0151-public-go-o3k-reference-policy.md,
docs/adr/ADR-0160-service-topology-and-execution-boundaries.md,
docs/adr/ADR-0162-contract-first-staged-runner-validation.md,
docs/specs/SPEC-0021-cross-service-workflows-and-compensation.md,
docs/specs/SPEC-0022-service-api-baseline-and-evidence-gates.md,
docs/specs/SPEC-0025-rust-rewrite-and-architecture-convergence.md,
contracts/execution-boundaries.md, and
contracts/core-architecture-boundaries.toml.

OpenStack public specifications and public client behavior are normative for
compatibility. Public Go O3K may be inspected only as a non-normative source of
requirements, failure scenarios, and operational lessons. Do not mechanically
translate Go code, preserve Go package structure, or reproduce Go database
layout.

Work from one coherent issue. If an existing open issue already owns the
acceptance criteria, extend that issue rather than creating micro-issues. If no
issue owns the scope, create one coherent issue before implementation.

Do not expand endpoint breadth. Preserve current public behavior unless the
issue explicitly changes an accepted compatibility record. Add tests before or
with implementation. Report uncertainty instead of inventing behavior.

Required validation before completion:
- python3 scripts/check-architecture-boundaries.py
- cargo fmt --all -- --check
- cargo clippy --workspace --all-targets --all-features -- -D warnings
- cargo test --workspace --all-features
- the closest contract/TestLab tests named by the changed subsystem.

Do not mark protected-host, release, compatibility, or product evidence as
passed unless it actually ran at the required evidence tier.
```

---

## Prompt 1 — canonicalize compute/server domain state

```text
Objective: make o3k-domain the single canonical owner of O3K server identity,
lifecycle state, and lifecycle transition invariants without changing the
public Nova-compatible behavior.

Current architecture debt to inspect:
- crates/o3k-domain currently has ServerId and ServerState but is not the
  canonical type consumed by the compute application.
- crates/o3k-compute defines its own Server with status: String.
- crates/o3k-provider defines provider InstanceState.
- crates/o3k-store persists desired/observed state as serialized values.

Implement the smallest coherent convergence:
1. Define canonical compute/server domain types in o3k-domain for the durable
   O3K identity and lifecycle semantics needed by the current TestLab profile.
2. Keep provider state separate, but add an explicit, tested projection from
   provider observations to canonical domain state.
3. Keep Nova/OpenStack status strings in o3k-api; add explicit, tested
   projection from canonical domain state to the current Nova response shape.
4. Keep persistence encoding in o3k-store; conversion to/from storage values
   must fail closed on unknown/corrupt state.
5. Remove free-form server lifecycle semantics from application code where the
   canonical domain type now exists.
6. Do not redesign flavors, keypairs, volumes, or networking beyond what is
   needed to make server lifecycle canonical.
7. Do not add endpoints or microversions.

Tests required:
- complete canonical transition table;
- invalid transition rejection;
- provider-observation -> canonical state projection;
- canonical state -> Nova status projection;
- corrupt/unknown persisted state fails closed;
- existing fake-provider TestLab server lifecycle remains behaviorally
  unchanged.

Acceptance:
- there is one canonical O3K server lifecycle model;
- API strings, persisted values, and provider states are projections, not
  competing state machines;
- no new architecture-boundary exception is added.
```

---

## Prompt 2 — extract repository ports and remove direct SqliteStore use

```text
Objective: remove direct SqliteStore coupling from o3k-compute and o3k-identity
while keeping SQLite as the only supported database adapter.

Do NOT implement PostgreSQL in this task.

Current debt is explicitly listed in
contracts/core-architecture-boundaries.toml:
- crates/o3k-compute/src/lib.rs
- crates/o3k-identity/src/lib.rs

Design narrow ports around actual use cases, not one enormous generic store.
Possible boundaries include IdentityRepository, ComputeRepository,
OperationRepository, or smaller capability traits where that produces clearer
transactions. Reuse an existing generic DurableStore trait only where its
semantics already match; do not force unrelated service state through a single
mega-interface.

Implementation requirements:
1. Application services depend on repository traits, not SqliteStore.
2. SqliteStore implements the required traits in the persistence adapter.
3. o3kd remains the composition root that chooses SQLite.
4. Preserve transaction, operation-journal, WAL, busy-timeout, restart, and
   idempotency behavior.
5. Remove the two concrete-store debt exceptions from the architecture
   contract as the coupling disappears.
6. Do not add PostgreSQL feature flags, fake support claims, or SQL abstractions
   that have no current behavioral test.
7. Do not change public OpenStack API behavior.

Tests required:
- repository conformance for extracted ports;
- identity restart/token behavior remains stable;
- compute create/lifecycle/delete operation persistence remains stable;
- concurrent SQLite behavior from issue #423 remains passing;
- architecture-boundary ratchet passes with a smaller debt list.

Acceptance:
- o3k-compute and o3k-identity no longer name SqliteStore in production
  application code;
- SQLite is still the real supported adapter;
- PostgreSQL remains planned, not implemented or claimed.
```

---

## Prompt 3 — make image metadata durable; keep image bytes out of SQLite

```text
Objective: move Glance-compatible image metadata and control-plane ownership
behind the durable repository boundary while preserving the existing bounded,
verified filesystem artifact implementation for image content.

Do not store complete image bytes in SQLite.

Required separation:
Durable store authority:
- image ID and project ownership;
- name/visibility/status;
- disk/container format;
- size/checksum/content identity;
- lifecycle metadata required for restart and API behavior.

Artifact/blob authority:
- uploaded bytes;
- content-addressed cache;
- qcow2 verification;
- compute-host base images and overlays;
- temporary publication files.

Implementation requirements:
1. Define a narrow image metadata repository port.
2. Preserve project visibility and upload/activation rules.
3. Keep checksum, regular-file, symlink/path, qemu-img, atomic publication, and
   cache revalidation protections already present.
4. Ensure restart reconstructs public image metadata from the durable store,
   not directory naming.
5. A missing/corrupt artifact for durable active metadata must fail closed and
   become observable; do not silently invent or reactivate bytes.
6. Preserve current Glance-compatible HTTP contracts.

Tests required:
- metadata survives restart independently of in-memory state;
- artifact bytes remain outside SQLite;
- missing/corrupt artifact behavior;
- upload interruption and atomic publication;
- project isolation;
- existing image contract/TestLab workflow.

Acceptance:
- filesystem paths are not public image identity;
- metadata and blobs have explicit different authorities;
- no new endpoint breadth.
```

---

## Prompt 4 — make Neutron intent and IP/MAC allocation durable

```text
Objective: move network/subnet/port control-plane metadata, allocation intent,
and host binding state behind durable repository ports while keeping host-local
TAP/bridge/DHCP execution behind NetworkProvider/agent ownership rules.

Durable control-plane authority must include the current TestLab subset:
- network/subnet/port IDs and project ownership;
- subnet CIDR/gateway/DNS/allocation data;
- deterministic MAC and fixed-IP allocation;
- port dependency and selected-host/binding intent;
- desired/observed binding state needed for restart/reconciliation.

Host execution remains responsible only for O3K-owned TAP/bridge/DHCP and
observations.

Requirements:
1. No public resource is reconstructed solely from a network metadata file.
2. Allocation changes are transactionally safe enough to avoid duplicate IP or
   MAC allocation under supported concurrency.
3. Restart preserves the same port/fixed-IP/MAC identity.
4. Foreign links and bridges remain protected by existing ownership fences.
5. Unknown execution outcome does not allocate a new port identity or fixed IP.
6. Do not add routers, floating IPs, security groups, VLAN/VXLAN, OVS, or OVN.

Tests required:
- concurrent fixed-IP allocation conflict;
- restart identity preservation;
- duplicate/retry behavior;
- host execution observation projection;
- delete/cleanup and foreign-link preservation;
- current Neutron TestLab contract tests.

Acceptance:
- control-plane network intent is durable-store authoritative;
- host manifests prove execution ownership only;
- first-alpha flat-network behavior is unchanged publicly.
```

---

## Prompt 5 — persist Placement through a repository boundary

```text
Objective: make Placement provider inventory, generations, usage, and
allocations durable through a repository port suitable for restart and later
multi-host work, without adding public Placement breadth.

Requirements:
1. Preserve current VCPU, MEMORY_MB, DISK_GB semantics.
2. Preserve generation conflict behavior and allocation idempotency.
3. Scheduler decisions use durable current inventory and allocations.
4. Server create persists selected provider/allocation identity before provider
   mutation as required by SPEC-0021.
5. Restart must not forget an allocation and schedule a duplicate server.
6. Keep SQLite as the implementation; do not add PostgreSQL in this task.
7. Do not add edge/HA claims.

Tests required:
- generation conflict;
- capacity exhaustion;
- allocation create/show/delete and exactly-once release;
- restart with active allocation;
- unknown provider outcome retains allocation;
- concurrent scheduler attempts do not over-allocate;
- existing TestLab scheduler/Placement workflow.

Acceptance:
- Placement state required for recovery no longer depends on file publication
  as the sole source of truth;
- no compatibility claim expands.
```

---

## Prompt 6 — remove agent/protobuf/Cinder adapter leakage from application code

```text
Objective: shrink the remaining adapter_dependency_debt entries in
contracts/core-architecture-boundaries.toml.

Current debt to inspect:
- o3k-compute depends directly on o3k-compute-agent,
  o3k-provider-contract/prost, and o3k-cinder.
- o3k-reconciler reads provider-contract wire events directly.

Do not rewrite the wire protocol. Do not rewrite stable safety code merely for
style.

Design bounded application-level ports/types for:
- compute command dispatch and observations;
- artifact availability/transfer intent;
- external volume-attachment service operations used by Nova;
- reconciliation inputs that should be independent of protobuf-generated
  messages.

Keep protobuf/tonic/agent-stream conversion in adapter code. Keep external
Cinder request/response models in the Cinder adapter.

Requirements:
1. Application logic remains responsible for operation identity, desired state,
   compensation, and unknown-outcome decisions.
2. Adapters remain responsible for transport/wire/provider translation.
3. Secret-bearing attachment information keeps existing redaction and bounded
   authenticated transport guarantees.
4. Remove architecture debt entries only when the corresponding dependency is
   actually gone.
5. Do not expand external-Cinder scope.

Tests required:
- provider fake/conformance behavior unchanged;
- stale epoch/generation and unknown outcome;
- agent reconnect/replay;
- Cinder attachment fake behavior where already supported;
- architecture-boundary ratchet shrinks.

Acceptance:
- application semantics can be tested without protobuf or external Cinder wire
  models;
- transport adapters can change without redefining lifecycle semantics.
```

---

## Prompt 7 — split o3k-api internally without behavior changes

```text
Objective: split crates/o3k-api/src/lib.rs into clear internal protocol-adapter
modules without changing any public route, request, response, status code,
header, microversion, or error behavior.

Target shape can be similar to:
- lib.rs / router composition
- error.rs
- auth/policy adapter helpers
- identity/
- image/
- network/
- compute/
- placement/
- volume_attachment/ only for already-declared behavior

Do not create one workspace crate per OpenStack service in this task.

Requirements:
1. Axum types and OpenStack JSON stay in o3k-api.
2. Domain/application crates must not depend on o3k-api.
3. Preserve route registration exactly.
4. Preserve operation-scoped microversion behavior exactly.
5. Preserve error envelopes and project-scope behavior.
6. No endpoint additions or "cleanup" behavior changes.

Tests required:
- existing o3k-api tests unchanged or minimally relocated;
- capability inventory generation unchanged;
- TestLab API baseline unchanged;
- cross-implementation contract fixtures unchanged;
- OpenStack CLI portable workflow unchanged.

Acceptance:
- this is a structural refactor only;
- diff is reviewable by service concern;
- no compatibility evidence is upgraded merely because files moved.
```

---

## Prompt 8 — close the native ephemeral-root alpha vertical slice

```text
Objective: make the native-rust-testlab v0.2.0-alpha.1 workflow the sole
release-blocking engineering focus and close the remaining real-host gaps using
existing issues/evidence gates.

First inspect the current open release-critical issues and protected evidence.
Do not guess which gates remain. Produce a table of:
- requirement;
- implementation state;
- portable evidence;
- component real-host evidence;
- full-profile evidence;
- latest source-bound result;
- blocking defect/issue.

The required workflow is:
discover/authenticate
-> upload/activate CirrOS
-> network/subnet/port
-> flavor/keypair
-> Placement allocation
-> server create through o3k-compute
-> verified image/config-drive/network realization
-> real libvirt/QEMU guest
-> ACTIVE + console + guest config-drive consumption
-> stop/start/hard reboot
-> restart o3kd/o3k-compute/libvirt as supported
-> reconcile same server identity
-> delete
-> prove no owned leak and no foreign-state change
-> clean install/reset/reinstall/uninstall/purge
-> benchmark + human review + release evidence.

Rules:
1. Fix the first failing release-critical boundary, not adjacent features.
2. External Cinder/Tempest work is non-blocking unless the defect is in a shared
   contract required by the native workflow.
3. Do not add native Cinder, PostgreSQL, HA, metadata HTTP, advanced Neutron,
   or extra Nova breadth.
4. Protected runner failures must be reproduced at the cheapest applicable
   layer before modifying the full runner when practical.
5. A timeout/transport loss after mutation is unknown outcome; observe before
   retry.
6. Preserve foreign-state fences and bounded redacted evidence.

Acceptance:
- packaging/release gate reports real source-bound readiness only when every
  required artifact genuinely passed;
- no skipped/fake/portable result is promoted to real-host evidence;
- release notes state the narrow TestLab alpha honestly.
```

---

## Prompt 9 — after alpha, mine Go O3K by user journey, not route count

```text
Run this only after the native ephemeral-root alpha is stable or when a human
maintainer explicitly requests compatibility inventory work.

Objective: expand the Rust compatibility backlog using public Go O3K and real
client behavior without creating a route-parity project.

Pin the exact public Go O3K commit inspected. Build a machine-readable inventory
for candidate behavior grouped by user journey:
- OpenStack CLI;
- openstacksdk;
- Terraform OpenStack provider;
- Horizon;
- operator/install/backup/diagnostic workflow.

For every candidate record:
- user outcome/client command;
- official OpenStack source;
- Go route/path consulted;
- Rust current status;
- whether the operation is already in a product profile;
- failure seen with the selected client;
- priority: blocks a declared journey / useful later / intentionally omitted;
- required spec/contract/test before implementation.

Do not implement routes in this task unless a separately accepted issue owns
one bounded compatibility change.

Acceptance:
- route count is reported only as inventory, never as progress or release
  percentage;
- no Go architecture/package/database structure is proposed for reuse;
- the output tells maintainers which client journey to improve next.
```

---

## Prompt 10 — recover selected operational product behavior from Go

```text
Run this after the native TestLab functional/recovery path is stable.

Objective: independently reproduce the product/operator outcomes that made Go
O3K easy to evaluate, without translating implementation structure.

Inventory and prioritize:
- zero-config first start;
- one-line/package installation;
- systemd service lifecycle;
- generated admin credential handling;
- health/readiness;
- metrics and tracing;
- backup/restore/upgrade;
- signed release/checksums/SBOM/provenance;
- reset/uninstall/purge safety;
- operator diagnostics.

For each outcome:
1. inspect the public Go behavior and operational lessons;
2. express the Rust requirement independently;
3. identify the Rust architecture owner and product profile;
4. add acceptance/failure/security tests;
5. implement the smallest Rust-native design;
6. measure the result.

Do not reintroduce Redis, RabbitMQ, service daemons, or other Go dependencies
merely because they exist in the reference implementation. Every dependency
must be justified by the Rust product requirement.

Acceptance:
- operational behavior is source-bound and tested;
- installer/cleanup cannot mutate foreign state;
- no product claim exceeds measured evidence;
- the Rust architecture remains simpler than the reference implementation.
```
