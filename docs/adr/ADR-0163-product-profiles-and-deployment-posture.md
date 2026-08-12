# ADR-0163 — Product profiles and deployment posture

Status: Accepted
Date: 2026-08-04
Decision-accepted: 2026-08-12
Human-approval: Senol Colak, 2026-08-12
Supersedes: none
Superseded-by: none
Affected-services: governance

## Context

O3K is one Cloud Operating System with multiple deployment, compatibility, and
evidence profiles.

Earlier wording risked presenting the profiles as three separate product
identities:

1. OpenStack service testbed;
2. native Rust OpenStack-compatible cloud;
3. small edge cloud.

ADR-0165 now establishes the stronger product architecture:

> O3K is a lightweight, open, Rust-native Cloud Operating System. OpenStack is
> a first-class compatibility contract, while the O3K Cloud Kernel is the
> canonical internal platform.

The profile system remains necessary because a TestLab, an external-service
testbed, and a multi-host edge cloud have different dependencies, maturity,
database, footprint, security, and evidence requirements.

Without explicit profiles, a real external Cinder deployment may be mistaken
for O3K implementing Cinder; a single-node SQLite TestLab may be mistaken for a
supported HA cloud; or a measured O3K process footprint may be confused with the
footprint of an entire hosted-service environment.

## Decision

### 1. O3K has one product identity and three primary deployment/evidence profiles

The product identity is O3K Cloud OS.

The primary profiles are:

#### A. OpenStack service testbed

O3K provides selected OpenStack-compatible surrounding APIs and Cloud Kernel
capabilities needed to run and test an independently operated OpenStack service
without deploying a complete DevStack/full control plane.

Example: external Cinder may authenticate through the selected
Keystone-compatible surface, validate tokens, discover its endpoint through the
compatibility catalog, access selected image/compute surfaces, and participate
in attachment workflows.

The hosted service keeps its own supported database, message bus, processes,
backend, migrations, upgrades, and operational ownership.

This profile is primarily a compatibility/developer product profile. It does not
define O3K's internal architecture.

#### B. Native O3K cloud

O3K owns the canonical Cloud Kernel resource state, policy/authorization,
operations, scheduling, reconciliation, and selected cloud-service domains.

OpenStack compatibility adapters expose declared Keystone-, Glance-, Nova-,
Neutron-, Placement-, and Cinder-compatible behavior where selected.

The first native real-cloud milestone remains an ephemeral-root libvirt TestLab.

Future O3K-native services may exist without an equivalent historical OpenStack
service. Such services still consume the same Cloud Kernel contracts and require
their own product/evidence profiles before support is claimed.

#### C. Small edge cloud

O3K operates as a lightweight multi-host Cloud OS targeting approximately
10–20 hypervisors in the initial edge profile.

The topology uses `o3kd` as the control plane plus typed host-local execution
boundaries such as `o3k-compute`, later `o3k-network`, and later `o3k-storage`.

The edge profile may integrate selected external services or clouds only through
explicit authority/trust profiles. "Connect to another OpenStack" is not one
feature.

### 2. Profiles share the Cloud Kernel, not separate cloud semantics

All profiles share, where selected:

- canonical O3K identities and resource ownership;
- O3K IAM/AuthContext contracts;
- authorization action/resource semantics;
- operation journal and reconciliation;
- compatibility manifests;
- provider/execution-boundary contracts;
- source-bound evidence;
- strict durable-ID versus display-name separation;
- release-claim discipline.

A capability verified in one profile is not automatically verified in another.

### 3. OpenStack compatibility claims remain operation-level

"Next-generation OpenStack" is product vision, not a blanket compatibility
claim.

Every OpenStack claim remains:

- service-specific;
- operation-specific;
- version/microversion-specific;
- evidence-backed;
- profile-specific.

O3K must not advertise full parity with a named OpenStack release merely because
its internal architecture is intended as a successor design.

### 4. SQLite is the default supported TestLab database

SQLite remains the default for the minimal TestLab and portable profiles.

Support requires explicit WAL/concurrency, crash recovery, migration,
backup/restore, and filesystem constraints.

A single-controller edge profile may use SQLite only within measured and
published limits.

### 5. PostgreSQL is the intended production-oriented database profile

PostgreSQL remains the intended database for production-oriented,
stronger-availability, or possible multi-controller profiles.

Until a real adapter and conformance evidence exist, documentation must say
`planned` or `production-profile target`, not `supported production database`.

### 6. The approximately 50 MB control-plane footprint is a measured target

The minimal O3K control plane targets approximately 50 MB steady-state memory.

This is not a blanket guarantee.

Every number identifies:

- profile;
- included O3K processes;
- source commit/build;
- host/kernel;
- workload phase;
- measurement method;
- excluded external dependencies.

External Cinder, RabbitMQ, PostgreSQL, libvirt, QEMU guests, Ceph, LVM, and
other dependencies are reported separately.

### 7. Existing-cloud federation is a separate future profile family

An existing cloud control plane is not an execution provider.

External OpenStack, vSphere/vCenter, Proxmox, KubeVirt, or public-cloud
integration requires a separately accepted delegated/federated connector model.

No generic cross-cloud claim is permitted until authority mapping, identity,
resource ownership, drift, outage, retry, and policy semantics are specified and
tested.

### 8. The current first-alpha release gate remains unchanged

The first release remains a libvirt TestLab alpha.

ADR-0165/0166 may guide code boundaries, but broad Cloud Kernel expansion,
native APIs, richer tenancy, new platform services, federation, or production
database work must not enter the first-alpha release critical path without a
separate explicit replan.

### 9. Release claims fail closed

No release may claim:

- full OpenStack release parity without operation-level evidence;
- production Cloud OS readiness merely from this architectural decision;
- PostgreSQL support without an implemented and verified adapter;
- metadata HTTP when config-drive is the only selected mechanism;
- native Cinder support when only external Cinder is hosted;
- edge-production readiness without database/restart/security/failure/
  operational evidence;
- a fixed 50 MB footprint without measurement;
- generic federation/cross-cloud support.

## Consequences

### Positive

- O3K gains one coherent product identity: the Cloud OS.
- The useful existing profile/evidence discipline remains intact.
- OpenStack service testbeds no longer define the product architecture.
- Native and future cloud services can share the Cloud Kernel.
- Release wording can be ambitious about direction while precise about current
  maturity.
- Database/footprint/external-service claims remain honest.

### Negative

- Documentation/tooling must distinguish product identity from deployment and
  compatibility profiles.
- Cloud Kernel features need independent maturity/evidence states.
- Future non-OpenStack-native services introduce new profile and API governance.
- The Cloud OS vision can be over-marketed unless release claims remain
  aggressively evidence-bound.

## Rejected alternatives

### Keep three profiles as three product identities

Rejected because it obscures the central O3K Cloud OS architecture and makes the
project look like a collection of related tools.

### Rename the native profile to "full OpenStack replacement"

Rejected because O3K intentionally supports selected compatibility profiles and
does not require complete upstream parity.

### Treat every existing cloud as another execution backend

Rejected because delegated clouds own scheduling, identity, quotas, policy, and
resource lifecycle and therefore require different authority semantics.

### Let the Cloud OS vision override release evidence

Rejected because architecture is intent, not proof.

## Required follow-up

- SPEC-0024 records the same profile/claim distinction;
- `compatibility/product-profiles.yaml` records accepted profile status;
- ADR-0165 remains normative for Cloud OS/Cloud Kernel identity;
- ADR-0166/SPEC-0020 remain normative for IAM and Keystone compatibility;
- release notes continue to identify the exact profile and evidence level;
- future first-class O3K services receive their own explicit product/evidence
  profile before support is advertised.
