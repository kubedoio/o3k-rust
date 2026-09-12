# ADR-0184 — P15 Scale and Composition Foundation

Status: Accepted
Date: 2026-09-12
Human-approval: explicit human architecture approval for ADR-0184, SPEC-0047, and the P15.0 architecture direction, granted 2026-09-12 in the PR #938 review (authorizes P15.1 after P15.0 merge)
Supersedes: none
Superseded-by: none
Affected-services: cloud-kernel

Related issues: [#929](https://github.com/o3kio/o3k/issues/929) (umbrella), [#930](https://github.com/o3kio/o3k/issues/930) (P15.0), [#931](https://github.com/o3kio/o3k/issues/931)–[#937](https://github.com/o3kio/o3k/issues/937) (P15.1–P15.7)
Normative specification: [SPEC-0047](../specs/SPEC-0047-p15-scale-and-composition-foundation.md)
Advances (does not amend): [ADR-0182](ADR-0182-edge-to-datacenter-building-block-cloud-os.md) with [SPEC-0039](../specs/SPEC-0039-edge-to-datacenter-building-block-cloud.md)

## Human approval

This ADR was **Accepted** on 2026-09-12 by explicit human architecture
approval, recorded in the PR #938 review. Under the ADR lifecycle
([ADR-0154](ADR-0154-engineering-governance-lifecycle.md)), architecture
decisions require explicit human approval before `Accepted`; that approval is
now recorded. Consequences:

- P15.1 (#931) is authorized once P15.0 (#930) is correctly merged.
- P15.2–P15.7 (#932–#937) proceed per phase under the accepted architecture
  via the protected review-and-merge loop
  ([docs/prompts/p15/REVIEW_AND_MERGE.md](../prompts/p15/REVIEW_AND_MERGE.md)),
  contingent on their listed dependencies being merged with evidence.
- Acceptance does **not** mark any implementation gap CLOSED: architecture
  approval ≠ implementation completion. E2D gap statuses change only with
  landed profile-scoped evidence, per the gap register rules.

## Context

### Post-#928 Cloud Kernel maturity

The P14/Araf convergence work (#887–#907, landed via PRs #889–#928) matured
the Cloud Kernel well beyond the P14 baseline. Current `main`, re-based for
P15.0 at `21fe687c` and audited in
[docs/architecture/p15-0-post-araf-current-state-audit.md](../architecture/p15-0-post-araf-current-state-audit.md),
now has: canonical Region/AZ location discovery
([ADR-0181](ADR-0181-canonical-location-identity.md),
[SPEC-0038](../specs/SPEC-0038-canonical-location-discovery-v1.md)); native
action-schema discovery
([SPEC-0040](../specs/SPEC-0040-native-resource-action-schemas.md)); bounded
list/query support (#906); durable canonical Operations (#898); relationship
reads (#899); domain actions (#897); generic update (#905); native quota
([SPEC-0043](../specs/SPEC-0043-native-quota-v1.md)); IAM governance
([SPEC-0044](../specs/SPEC-0044-native-iam-governance-v1.md)); durable audit
([SPEC-0042](../specs/SPEC-0042-durable-audit-v1.md)); operator diagnostics
([SPEC-0045](../specs/SPEC-0045-native-operator-diagnostics-v1.md)); and
authoritative metering
([ADR-0183](ADR-0183-authoritative-metering-and-bounded-usage-aggregation.md),
[SPEC-0046](../specs/SPEC-0046-native-metering-v1.md)). The #907/#928 gate
proved northbound convergence with a **real** `o3kd` process, real OIDC, and
real durable stores — but with the fake execution provider.

### E2D gap state

Per [docs/architecture/p15-e2d-gap-register.md](../architecture/p15-e2d-gap-register.md),
the ADR-0182 register E2D-01..E2D-18 stands at: E2D-01 PARTIAL, E2D-02
PARTIAL, E2D-03 OPEN, E2D-04 OPEN, E2D-05 OPEN, E2D-06 PARTIAL, E2D-07 OPEN,
E2D-08 PARTIAL, E2D-09 PARTIAL, E2D-10 OPEN, E2D-11 PARTIAL, E2D-12 OPEN,
E2D-13 PARTIAL, E2D-14 PARTIAL, E2D-15 PARTIAL, E2D-16 PARTIAL, E2D-17 OPEN,
E2D-18 PARTIAL. **No gap is closed by API existence alone**: an endpoint,
table, or module landing on `main` moves a gap's status only when
profile-scoped evidence lands with it.

### The #928 evidence boundary

The #907/#928 northbound convergence gate (evidence ledger:
[docs/evidence/ISSUE_907_ARAF_P2_NORTHBOUND_CONVERGENCE.md](../evidence/ISSUE_907_ARAF_P2_NORTHBOUND_CONVERGENCE.md),
still DRAFT with frozen-head placeholders per #903/#904/#907) proves that
Araf-consumable northbound truth is durably convergent across a real process,
a mid-journey restart, and both durable stores (PostgreSQL and SQLite). It
proves **nothing** about execution at scale: the compute provider was
`O3K_PROVIDER=fake`, the topology was a single control-plane process on
loopback, and no building block, hierarchical placement, or multi-host
behavior was exercised.

## Decision

### 1. P15 is the Scale & Composition Foundation program

P15 is defined as the Scale & Composition Foundation program. Purpose:
"Extend the proven O3K Cloud Kernel with the topology, composition, Placement,
deployment-building-block and bootstrap foundations necessary for one O3K
architecture to grow from edge deployments toward datacenter deployments
without changing canonical cloud semantics or creating a second product
architecture."

P15 is **not** the datacenter-scale proof. It builds the foundations whose
evidence-backed scale claims come later (P18 and beyond).

### 2. Phase structure and dependency order

```text
P15.0 re-baseline
  -> P15.1 canonical topology/failure domains
  -> P15.2 service-registry authority convergence (P15.1 and P15.2 mutually independent)
  -> P15.3 hierarchical/capability-aware Placement (needs P15.1)
  -> P15.4 declarative CloudProfile/service composition (needs P15.2)
  -> P15.5 deployment Building Block lifecycle (needs P15.1+P15.3+P15.4)
  -> P15.6 production init + authenticated join (needs P15.4+P15.5)
  -> P15.7 scale/composition convergence and real-host evidence gate (needs all)
```

Phases P15.1–P15.7 are tracked by issues #931–#937; P15.0 by #930; the
umbrella is #929. A phase's dependencies must be merged with their own
evidence before dependent work cites them.

### 3. Building Blocks compose and link; they never duplicate authority

Building Blocks compose/link existing canonical authorities — topology
(P15.1), Placement (P15.3), CloudProfile (P15.4), and execution identities.
They never become a second scheduler, a second capacity database, a second
topology authority, or a duplicate agent inventory.

### 4. Placement consumes topology constraints; topology does not schedule

Target placement model: ResourceProvider with inventories, allocations,
parent/child topology, capabilities/traits, and failure-domain references.
Candidate selection supports quantity, required/forbidden capabilities,
location constraints, failure-domain constraints, and locality — preserving
durable allocations, determinism, idempotency, generation fencing, and
unknown-outcome semantics.

### 5. One authoritative native service/manifest state

Native service/manifest state converges to a single authority (converging
KernelRegistry and ManifestRegistry in P15.2). The OpenStack catalog and
native discovery are projections derived from it. Catalog registration does
not install software.

### 6. CloudProfile is the declarative desired-composition contract

CloudProfile (name not final) is the declarative desired-deployment-composition
contract, semantically separated from ManifestRegistry (observed/exists) and
from the catalog (consumable projection). It holds no tenant resource state.

### 7. Cells/sharding/partitioning only where measurements require them (P18)

Cells, sharding, and partitioning are permitted only where measurements
require them; that evaluation belongs to P18. None are introduced in P15.

### 8. Post-P15 program map (planning, not accepted architecture)

- P16 Site Autonomy/Datacenter Fabric/Workload Mobility (E2D-07/08/09/16);
- P17 Production Operations & Trust (E2D-11 remainder/12/14/15);
- P18 Measured Datacenter Scale (E2D-10/17);
- P19 Hosted Ecosystem (E2D-06, Octavia, Designate, Barbican, later selected
  services).

This map is planning context for sequencing and claim governance only; it is
not accepted architecture, and each program requires its own ADR.

### 9. Claim governance extension

Claim governance extends the
[docs/status/current-state.yaml](../status/current-state.yaml) +
[scripts/validate-profile-state.py](../../scripts/validate-profile-state.py)
machinery; issue #433 stays the tracking issue. README/roadmap/E2D-register
claims must be derived from or cross-checked against the machine-readable
evidence state. There is no second claim system.

## Cloud Kernel invariants

These invariants hold across P15 and are restated normatively in SPEC-0047 §5:

1. **One canonical cloud authority.** Native API, OpenStack compatibility,
   Araf, Terraform/OpenTofu, and Building Blocks all consume or project the
   same O3K authority; none creates a parallel one.
2. **Building Blocks are not a second Placement.** Capacity and scheduling
   truth live only in Placement.
3. **Topology is not a scheduler.** Location/failure-domain truth is
   descriptive and referential; scheduling decisions belong to Placement.
4. **CloudProfile is desired composition, not runtime discovery.** Observed
   service truth and consumable catalog projections are separate states.
5. **Catalog registration does not install software.**
   Advertised-implies-executable is preserved through evidence, not
   registration.
6. **Compatibility is derived.** OpenStack catalog/API behavior is a
   projection of canonical O3K state, never an authority over it.
7. **Araf is not authoritative.** Araf discovers capabilities, schemas,
   topology, relationships, quota, metering, governance, and operations from
   O3K; no dashboard-specific truth lives in the kernel.
8. **Scale mechanisms follow measurements.** Cells/sharding/partitioning
   appear only at an observed bottleneck (P18), behind unchanged product
   contracts.
9. **WAN is not a local correctness dependency.** This future multi-site
   invariant (E2D-07) is preserved by P15's design and explicitly not solved
   in P15.

## Phase boundaries and dependency order

Per-phase dependencies are fixed in Decision 2. Each subsection below states
the phase's intended boundary and guardrails; the normative per-phase
contract, including acceptance criteria, is SPEC-0047 §4.

### P15.1 — canonical topology/failure domains (#931)

- Complete canonical topology beyond Region/AZ: a generic failure-domain
  model under Region/AZ.
- Bindings of failure domains to providers/hosts/fabric/storage by reference.
- Fix `seed_core` empty locations; Keystone region/AZ projection derived from
  canonical topology.
- Guardrails: extend LocationRegistry (no second authority); no
  provider-specific topology in public semantics; no hard-coded rack/row
  model unless an accepted contract requires it; no scheduler inside
  topology; no Araf-owned topology.

### P15.2 — service-registry authority convergence (#932)

- Converge service authority to a single native authority (KernelRegistry/
  ManifestRegistry convergence).
- Catalog and native discovery derived from that authority.
- Safe migration path for existing consumers; advertised-implies-executable
  preserved throughout.

### P15.3 — hierarchical/capability-aware Placement (#933)

- Hierarchical/capability-aware Placement per Decision 4: ResourceProvider
  inventories, allocations, parent/child topology, capabilities/traits,
  failure-domain references.
- Candidate selection: quantity, required/forbidden capabilities, location
  constraints, failure-domain constraints, locality.
- Guardrails: no cells without measurements; provider-neutral model;
  preserves durable allocations, determinism, idempotency, generation
  fencing, unknown-outcome semantics.

### P15.4 — declarative CloudProfile/service composition (#934)

- Declarative CloudProfile desired composition: selected services, ownership
  mode, versions, dependencies, required capabilities, placement/locality
  constraints, config references, upgrade ordering.
- Drift between desired and observed state is surfaced via canonical
  Operations.
- External-hosted services retain their own lifecycle; CloudProfile is not
  the runtime catalog.

### P15.5 — deployment Building Block lifecycle (#935)

- First-class BuildingBlock lifecycle: enrollment, identity, capability
  publication, failure-domain membership.
- Administrative states Ready/Unavailable/Draining; removal/replacement with
  honest drain blockers.
- Capacity projection derived from Placement; operator/Araf visibility.

### P15.6 — production init + authenticated join (#936)

- `o3k init`, CloudProfile select, `o3k join`: identity, certificates,
  provider/agent registration, Placement publication, failure-domain
  assignment, profile reconciliation, readiness, client/Araf config
  generation.
- Bootstrap timing claims require benchmark evidence and must exclude
  pre-provisioned external work O3K does not perform.

### P15.7 — scale/composition convergence and real-host evidence gate (#937)

- Real convergence/evidence phase over the full journey: fresh deploy ->
  init -> profile -> enroll multiple real blocks -> topology appears ->
  capacity appears -> workload -> topology/capability placement enforced ->
  add block -> drain block with honest blockers -> remove/rejoin/replace ->
  restart control plane and PostgreSQL -> IDs/topology/profile survive ->
  native API authoritative -> OpenStack projection convergent -> Araf
  consumes the same truth.
- Real execution boundary throughout; the #928 fake provider is not
  sufficient for P15.5–P15.7 execution claims.
- Proves the building-block architecture, not the final scale ceiling;
  blockers are exposed honestly, not hidden.

## Authority boundaries

Each concept has exactly one authority; everything else derives from it or
links to it by reference.

| Concept | Single authority | Derivers / linkers |
| --- | --- | --- |
| Topology / failure domains | LocationRegistry extension (P15.1, per ADR-0181) | Keystone region/AZ projection; Araf; Building Block membership references |
| Scheduling / capacity / allocations | Placement (P15.3) | Building Block capacity projection; CloudProfile placement constraints |
| Desired deployment composition | CloudProfile (P15.4) | Profile reconciliation; init/join flows |
| Observed / existing services | Converged native service/manifest registry (P15.2) | Native discovery; OpenStack catalog projection |
| Compatibility projections | Derived only (never authority) | Catalog, endpoints, microversion ranges per SPEC-0022 |
| Metering | Kernel metering authority ([ADR-0183](ADR-0183-authoritative-metering-and-bounded-usage-aggregation.md)) | Araf/tenant usage reads |
| Audit | Durable audit ([SPEC-0042](../specs/SPEC-0042-durable-audit-v1.md)) | Correlation consumers |
| Building Blocks | Lifecycle state only (P15.5) | Compose/link topology, Placement, CloudProfile, execution identities |
| Araf | Consumer only | Discovers; never owns topology/capability truth |

## Claim boundaries

### Permitted after P15.0

Only what existing evidence proves: the post-#928 kernel capabilities listed
in Context, each bounded by its own profile and evidence ledger — and every
execution-flavored statement carries the caveat that the #928 gate ran the
fake execution provider. The small-edge libvirt TestLab profile remains the
only real-execution evidence profile.

### Forbidden without specific new evidence

Datacenter scale; thousands of hosts; multi-region; production HA;
production live migration; production workload evacuation; zero-downtime
block maintenance; arbitrary OpenStack compatibility; "all OpenStack
services supported"; production bootstrap in seconds.

### Per-phase claim limits

Each phase may claim only what its own evidence tier proves. P15.7 proves
the building-block architecture — not the scale ceiling. Scale-ceiling
claims belong to the P18 measured-scale program and its evidence ladder.

## Non-goals

For P15, verbatim from the program:

- no runtime work in P15.0;
- no cells, sharding, multi-region execution, live migration, evacuation,
  EVPN/VXLAN/OVN, new Ceph behavior, Octavia, Designate, Barbican, Manila,
  object storage, Kubernetes-service, DBaaS or AI service APIs, billing,
  marketplace, organization hierarchy, or Araf UI.

Genuine defects found in current `main` are recorded separately (their own
issues), not silently fixed inside a P15 phase.

## Risks

- **Registry convergence breaks existing consumers (P15.2):** the migration
  path must keep Keystone/catalog behavior stable while authority moves;
  advertised-implies-executable must survive the cutover.
- **CloudProfile semantics creep into runtime discovery (P15.4):**
  desired-vs-observed conflation would recreate the E2D-05 gap inside the new
  contract.
- **Building Block absorbs placement/topology authority by accident
  (P15.5):** the block view must remain a projection/link layer over
  Placement and LocationRegistry.
- **Scope creep toward the datacenter proof:** P15 is foundations-only;
  datacenter-scale claims require P18 evidence.
- **Evidence ledgers still DRAFT (#903/#904/#907 pending placeholders):**
  claims must respect frozen-head semantics — no claim may rely on a
  placeholder a later commit invalidates.
- **Over-claiming from #928:** the fake-provider northbound gate must not be
  quoted as execution or scale evidence.

## Future consequences

- The post-P15 program map (Decision 8) becomes implementable once P15
  lands: P16 site autonomy/fabric/mobility, P17 production operations &
  trust, P18 measured datacenter scale, P19 hosted ecosystem.
- ADR-0182 remains the strategic decision; this ADR advances it and does not
  amend it.
- A future ADR may supersede the invariants above only explicitly, with a
  recorded supersession relationship.

## Alternatives considered

- **(a) Jump straight to cells/sharding.** Rejected: no measurements justify
  partitioning yet, and it violates the scale-mechanisms-follow-measurements
  invariant (Decision 7).
- **(b) Make the runtime catalog the desired-state document.** Rejected: it
  conflates observed with desired state — the exact E2D-05 gap P15.4 exists
  to close.
- **(c) Let Building Blocks own capacity/scheduling.** Rejected:
  second-scheduler/second-capacity-database violation of Decision 3 and the
  ADR-0182 review constraint 1.
- **(d) Self-accept this ADR.** Rejected: human architecture approval is
  required before `Accepted`; this record would have stayed Proposed until
  human approval. That approval was granted on 2026-09-12 (recorded in the PR
  #938 review).
