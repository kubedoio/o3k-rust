# SPEC-0038 — Edge-to-datacenter building-block cloud

Status: Normative
Version: 1.0.0
Related ADR: [ADR-0181](../adr/ADR-0181-edge-to-datacenter-building-block-cloud-os.md)

## Purpose

This specification turns the O3K end-to-end product direction into concrete
architecture and claim rules.

O3K is one Cloud Operating System intended to begin small at the edge and scale
to datacenter deployments without requiring a change of cloud product,
resource model, IAM model, API family, automation model, or service-discovery
model.

This specification does **not** assert that current small-edge evidence proves
large datacenter scale. Every supported scale remains separately evidence-bound.

## 1. Scale-continuity invariant

An O3K deployment moving from a smaller to a larger supported topology SHALL
preserve the following product contracts unless a separately accepted breaking
version explicitly says otherwise:

- canonical O3K resource identity and ownership semantics;
- O3K IAM, Principal, scope and AuthContext semantics;
- native O3K API family and versioning rules;
- selected OpenStack compatibility APIs and advertised operation profiles;
- durable Operation/idempotency/unknown-outcome/reconciliation semantics;
- service registry/catalog identity and ownership modes;
- Terraform/OpenTofu compatibility approach;
- provider-neutral typed execution boundaries;
- Araf capability/resource model.

Scaling SHALL NOT require a customer to export resources from an "edge product"
and import them into a separate "datacenter product" merely to gain capacity.

## 2. Deployment building block

A deployment building block is a capacity/capability contribution with explicit
failure-domain identity.

A block descriptor SHALL be able to represent, as applicable:

- block identity;
- region and availability/failure-domain identity;
- compute capacity/capabilities;
- network execution/fabric capabilities;
- storage execution/backend capabilities;
- administrative/availability state;
- control-plane participation where applicable;
- health/readiness/fencing generation;
- service capabilities relevant to placement or catalog behavior.

The descriptor SHALL NOT require all capabilities to be present on every block.

The architecture SHALL NOT define a building block as one mandatory hardware
SKU, one fixed number of hosts, or one fixed process topology.

## 3. Building-block lifecycle

For every profile that permits dynamic capacity growth, the supported lifecycle
SHALL define:

```text
prepare/enroll
-> authenticate
-> publish capability and failure-domain identity
-> become eligible for placement
-> drain
-> remove or replace
```

Where external side effects are possible, existing O3K rules continue to apply:

- durable intent before mutation;
- generation/epoch fencing;
- deterministic command/idempotency identity;
- unknown-outcome classification;
- observation before retry;
- ownership-safe cleanup;
- restart reconstruction;
- no mutation of foreign state.

## 4. Scale-specific internal topology

The scale-continuity invariant applies to product semantics, not to a fixed
internal implementation.

A larger deployment MAY introduce:

- scheduler cells or partitions;
- control-plane work partitions;
- database replication, partitioning or sharding;
- hierarchical capacity aggregation;
- multiple fabric domains;
- storage domains/backends appropriate to larger failure boundaries;
- regional API/control-plane placement;
- other scale mechanisms proven necessary by measurement.

Such mechanisms SHALL remain behind stable O3K contracts and SHALL NOT create a
second canonical IAM/resource/operation authority.

If a scale mechanism changes externally visible semantics, it requires a
separate accepted contract and compatibility impact analysis.

## 5. Scale profiles and evidence

O3K SHALL publish explicit supported scale profiles rather than one unbounded
"scales to the datacenter" claim.

Each scale profile SHALL record at least:

- tested real host count and any simulated count separately;
- control-plane topology;
- persistence/database topology;
- execution-provider topology;
- network/fabric topology;
- storage topology;
- failure-domain model;
- supported service catalog;
- maximum or measured capacity dimensions relevant to the claim;
- workload and reconciliation latency measurements where relevant;
- upgrade/restart/failure evidence;
- cleanup/foreign-state evidence;
- exact limitations.

A smaller profile does not prove a larger profile.

The existing small-edge profile is the first bounded rung of this continuum.
It is not the architectural maximum of O3K.

## 6. Service catalog composition

The O3K service catalog SHALL be composable per deployment profile.

Every service entry SHALL declare:

- service identity/type;
- endpoint(s) and region(s);
- capability/version information;
- ownership mode;
- health/readiness state;
- dependency requirements;
- support/evidence profile;
- whether the service is required or optional for the selected deployment
  profile.

At minimum the ownership vocabulary SHALL distinguish:

### Native O3K

O3K owns canonical resource identity, authorization, desired state, operations,
reconciliation and lifecycle.

### External-hosted

The external service owns its service-local state, database, workers, migrations
and service lifecycle. It consumes only declared O3K compatibility or native
contracts.

External-hosted state SHALL NOT be represented as O3K-native implementation
merely because the endpoint is registered in the O3K catalog.

Future delegated/federated ownership modes require separate normative contracts.

## 7. Minimum and customized service profiles

The product SHALL support an operator-selected service profile rather than
requiring every possible cloud service at every site.

A minimal IaaS-oriented profile may contain:

```text
Identity
Image
Compute
Network
Volume
```

Additional profiles may include, where implemented/proven:

```text
Load Balancing
DNS
Secrets
Object Storage
Metering / Usage
Kubernetes
Database
AI / GPU services
other specialized capabilities
```

The UI and clients SHOULD consume capability discovery instead of inferring
service availability from deployment size.

## 8. Upstream OpenStack services as optional extensions

OpenStack compatibility is also an ecosystem extension mechanism.

Before creating a native O3K implementation of a specialized service, the
project SHOULD evaluate upstream service hosting where appropriate.

Candidate examples include:

- Octavia;
- Designate;
- Barbican;
- Manila;
- CloudKitty;
- other maintained upstream services with a bounded dependency surface.

No service is considered supported merely because O3K exposes Keystone/Nova/
Neutron/Cinder-compatible endpoints.

For each hosted-service profile the project SHALL perform black-box dependency
contract discovery covering the exact upstream service version and the exact
O3K/OpenStack operations it consumes.

The conformance workflow SHALL be:

```text
pin upstream service version
-> observe/freeze required dependency behavior
-> compare with O3K compatibility profile
-> implement bounded missing behavior
-> run real service lifecycle
-> prove restart/failure/reconciliation/cleanup
-> register the supported service profile
```

Unsupported semantics SHALL fail closed rather than silently degrade.

## 9. Edge bootstrap and adoption target

O3K SHOULD make a prepared edge site quick to turn into a usable cloud by
minimizing operator steps required to:

1. initialize the selected control-plane profile;
2. establish IAM/bootstrap trust;
3. select a service catalog profile;
4. enroll prepared building blocks;
5. expose native and selected OpenStack-compatible endpoints;
6. produce client configuration for supported tooling.

A timing claim such as "seconds" or "minutes" SHALL define the measured start
and end boundary.

A benchmark MAY measure O3K bootstrap on already prepared infrastructure. It
SHALL NOT include pre-existing dependencies as if O3K provisioned them, and
SHALL NOT exclude slow dependencies while presenting the result as full-site
production readiness.

## 10. Edge-to-datacenter operator continuity

A supported scale transition SHOULD preserve operator workflows for:

- Araf;
- OpenStack CLI/SDKs within the advertised compatibility profile;
- OpenTofu/Terraform within the advertised compatibility profile;
- identity/scope selection;
- service discovery;
- host/block enrollment;
- capacity/failure-domain inspection;
- upgrades and drains;
- usage/metering where supported.

Scale-specific advanced/operator fields may be added without changing the
normal tenant resource vocabulary.

## 11. Datacenter-scale requirements

A future datacenter-scale claim requires evidence beyond correctness on a small
cluster.

At minimum the profile SHALL include:

- real multi-failure-domain deployment;
- production-oriented persistent store topology;
- concurrent controller/work-owner evidence where HA is claimed;
- sustained scheduling/reconciliation load;
- host/block churn;
- rolling upgrade and rollback;
- control-plane and execution-boundary partial failures;
- network and storage failure-domain evidence;
- capacity exhaustion behavior;
- recovery after controller/database/network interruption;
- independent leak/foreign-state verification;
- measured resource and latency budgets.

Large scale SHALL NOT be claimed solely from simulated agents or unit tests.
Simulation may supplement but not replace real profile evidence.

## 12. Product wording

Valid architectural wording:

> O3K is one Cloud Operating System from edge to datacenter. It scales by adding
> capability-bearing building blocks while preserving the same cloud authority,
> APIs, IAM, service catalog and automation model.

Valid qualified wording:

> O3K's current small-edge profile is one proven scale rung; larger datacenter
> profiles require their own evidence.

Valid ecosystem wording:

> O3K preserves selected OpenStack compatibility both for existing clients and
> as a bounded integration surface for supported upstream OpenStack services.

Invalid standalone wording without evidence includes:

- "O3K supports thousands of hosts";
- "O3K creates a production cloud in seconds";
- "any OpenStack service works automatically with O3K";
- "the same process topology scales from three hosts to every datacenter";
- "catalog registration means O3K implements the service".

## 13. Architecture fitness requirements

Future architecture changes touching deployment scale or service composition
SHALL be reviewed against these questions:

1. Does the change preserve one canonical O3K cloud authority?
2. Would a customer need to replatform merely because the deployment grew?
3. Are edge and datacenter differences implementation/profile concerns rather
   than separate product semantics?
4. Is the service ownership mode explicit?
5. Does capability discovery remain authoritative enough for UI/automation
   without replacing backend authorization?
6. Are scale and bootstrap claims bound to measured evidence?
7. Can optional upstream services be added without leaking their implementation
   models into the Cloud Kernel?

A change that violates these invariants requires an explicit successor ADR.
