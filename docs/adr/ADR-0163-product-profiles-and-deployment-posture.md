# ADR-0163 — Product profiles and deployment posture

Status: Proposed

Date: 2026-08-04

## Context

O3K is a Rust-native OpenStack-compatible control plane. The same codebase is
intended to serve three related but materially different product uses:

1. a lightweight testbed around selected real OpenStack services;
2. a native Rust implementation of declared OpenStack service profiles;
3. a small edge-cloud control plane for approximately 10–20 hypervisors.

Without explicit profiles, the project can make contradictory claims. An
external Cinder deployment may be mistaken for O3K implementing Cinder. A
single-node SQLite TestLab may be mistaken for a supported HA production
control plane. A measured binary or process footprint may be reported as the
footprint of an entire external-service environment.

The project also needs an honest database and resource-consumption posture.
SQLite is the current default implementation. PostgreSQL is the intended
production-oriented database profile, but must not be claimed as supported
until an adapter, migrations, conformance tests, backup/restore behavior, and
operational documentation exist. The often stated approximately 50 MB resource
usage is a product target that must be measured per profile rather than treated
as an unconditional fact.

## Decision

### 1. O3K has three first-class product profiles

#### OpenStack service testbed

O3K provides the surrounding OpenStack-compatible services required to run and
test a selected real OpenStack service without deploying a complete DevStack or
full OpenStack control plane.

Example: a real external Cinder deployment authenticates against O3K Identity,
registers a `volumev3` endpoint in the O3K catalog, validates tokens through the
public Identity API, consumes the declared Glance-compatible surface, and
participates in Nova attachment workflows.

The hosted service remains external. It keeps its own supported database,
message bus, service processes, storage backend, migrations, upgrades, and
operational responsibilities.

#### Native Rust cloud

O3K progressively implements its own Rust-native compatibility profiles for
Keystone, Glance, Nova, Neutron, Placement, and Cinder behavior.

Compatibility is operation-level and evidence-backed. O3K does not claim full
parity with an entire named OpenStack release merely because selected routes
exist.

#### Edge cloud

O3K supports a small cloud profile targeting approximately 10–20 hypervisors.
The profile uses `o3kd` as the control plane and typed host execution boundaries
such as `o3k-compute`, later `o3k-network`, and later `o3k-storage`.

The edge profile may interoperate with selected external OpenStack services or
identity/catalog infrastructure only through separately declared and tested
integration profiles. “Connect to another OpenStack” is not one feature; each
identity, catalog, service-hosting, resource-sharing, or federation behavior
requires its own contract and security decision.

### 2. Profiles share architecture, not claims

All profiles share:

- the Rust domain and application model;
- typed authorization context;
- operation journal and reconciliation;
- compatibility manifests;
- provider and execution-boundary contracts;
- source-bound evidence;
- strict separation of durable IDs from display names.

A capability verified in one profile is not automatically verified in another.
Catalog entries, release claims, footprint numbers, database support, and
security guarantees are profile-specific.

### 3. SQLite is the default supported TestLab database

SQLite is the default database for the minimal TestLab and portable profiles.
Its support requires explicit WAL/concurrency, crash-recovery, migration,
backup/restore, and filesystem constraints.

A single-controller edge profile may use SQLite only when the documented scale,
concurrency, durability, and availability limits are accepted and measured.

### 4. PostgreSQL is the intended production-oriented profile

PostgreSQL is the intended database for production-oriented, multi-controller,
or stronger availability profiles.

Until the PostgreSQL adapter and conformance evidence exist, documentation must
say that PostgreSQL is planned or required by the target production profile,
not that it is currently supported or recommended as an installable option.

### 5. The approximately 50 MB footprint is a measured target

The minimal O3K control plane targets an approximately 50 MB steady-state
memory footprint. This is not a blanket guarantee.

Every published number identifies:

- profile;
- included O3K processes;
- excluded external dependencies;
- source commit and build mode;
- host and kernel;
- measurement method;
- idle or workload phase.

External Cinder, RabbitMQ, PostgreSQL, libvirt, QEMU guests, storage backends,
and other hosted dependencies are reported separately from O3K process usage.

### 6. Release claims fail closed

No release may claim:

- PostgreSQL support without an implemented and verified adapter;
- full OpenStack release parity;
- metadata HTTP when config-drive is the only mechanism;
- native Cinder support when only an external Cinder endpoint is hosted;
- edge-production readiness without the selected database, restart, security,
  failure, and operational evidence;
- a 50 MB footprint without a source-bound measurement artifact.

## Consequences

### Positive

- testbed, native-cloud, and edge-cloud work can progress without being
  conflated;
- real OpenStack services can be integrated earlier without weakening the
  native Rust roadmap;
- database and footprint claims remain credible;
- compatibility and release evidence can be attached to a concrete profile;
- small TestLab deployments remain simple while production-oriented work has a
  clear upgrade path.

### Negative

- compatibility manifests and release tooling must understand multiple
  profiles;
- some features require separate evidence in more than one profile;
- PostgreSQL and edge-production claims remain unavailable until real adapters
  and operations work are completed;
- external-service testbeds still need the external service's own supported
  dependencies.

## Rejected alternatives

### Treat external services as O3K implementations

Rejected because catalog hosting and satellite API compatibility do not mean
O3K owns the external service API or runtime.

### Wait for complete native OpenStack parity before integration

Rejected because the service-testbed use case is valuable earlier and complete
upstream parity is not a bounded milestone.

### Use one database and footprint claim for every deployment

Rejected because TestLab, edge, native-storage, and hosted-service profiles have
different dependencies, concurrency, and resource consumption.

### Claim PostgreSQL support from an architectural intention

Rejected because architecture is not executable support evidence.

## Required follow-up

- define the profiles normatively in SPEC-0024;
- record them in `compatibility/product-profiles.yaml`;
- keep external Cinder behavior in SPEC-0023;
- implement database posture work in issue #423;
- implement footprint evidence in issue #431;
- validate release claims in issue #433;
- require a separate ADR for external Keystone, federation, or cross-cloud
  resource sharing.
