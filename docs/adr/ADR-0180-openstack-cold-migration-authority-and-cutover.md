# ADR-0180 — OpenStack Cold Migration Authority and Cutover

Status: Accepted
Date: 2026-09-06
Human-approval: P14.0 protected review-and-merge loop (issue #856)
Supersedes: none
Superseded-by: none
Affected-services: cloud-kernel, iam, image, compute, network, storage, compatibility, operations

Related issue: #856
Normative specification: [SPEC-0037](../specs/SPEC-0037-openstack-cold-migration-v1.md)

## Context

P13 freezes a bounded OpenStack compatibility profile, but compatibility is
not an adoption mechanism. P14 must move selected resources from an existing
OpenStack cloud into canonical O3K resources without treating an OpenStack
database, provider identity, or OpenStack service topology as O3K authority.
The migration must survive restart, retries, and unknown external outcomes.

Public reference inputs inspected on 2026-09-06:

- OS Migrate repository and README: https://github.com/os-migrate/os-migrate
- OpenStack compute migration documentation: https://docs.openstack.org/nova/latest/admin/migration.html

OS Migrate is an architectural reference only. O3K does not copy its source,
Ansible roles, schemas, or implementation. The public observations used here
are official-API-only transfer, no direct database access, retryable/idempotent
operations, and explicit source/destination prerequisites.

## Decision

Before cutover, the declared source OpenStack cloud is authoritative for the
source snapshot. O3K is authoritative for every destination object it creates
and for the durable migration operation. After an explicit, durable cutover
commit, O3K is authoritative for the migrated resource set. Source cleanup is
outside that transaction and is never automatic in v1.

Every source object is addressed by `(source_cloud_id, resource_type,
source_resource_id)`. It maps through a durable migration mapping to a newly
allocated canonical O3K ID. Source UUID reuse is forbidden by default. Source
IDs, names, metadata, and provider state are never destination authority.

The canonical O3K `AuthContext` authorizes each control-plane mutation using
the original actor, requested destination scope, destination resource, and
migration operation. Source credentials are scoped capability inputs, stored
only as protected references, and never copied into domain state, manifests,
logs, or destination resources.

The durable state machine is:

`planned -> preflighted -> manifested -> transferring -> validated ->
cutover-pending -> cutover-committed -> finalized`

Failure states are `blocked`, `rolled-back`, and `unknown-outcome`. A phase
transition records migration ID, monotonic generation, actor/scope, source
snapshot fingerprint, operation/idempotency IDs, and audit event before an
external side effect when recovery depends on that intent. On timeout or lost
response, observe source and destination before retrying. Workers resume only
from durable state and use fencing.

Cutover is one-way for the selected set. It requires fresh validation, source
quiescence evidence, and authorization. A failed pre-cutover migration may
compensate destination objects in reverse dependency order when ownership is
proven. After cutover, rollback is forward reconciliation under O3K authority.

P14.0 defines a bounded profile for project/scope, image, flavor mapping,
keypair public key, network, subnet, port, security group/rule, router, router
interface, floating/public IP, server, volume, and volume attachment. Images
and volume bytes cross the boundary through authenticated public API transfer
or a declared adapter; destination writes go only through canonical O3K APIs.
Unsupported extensions, encrypted-secret extraction, passwords, private keys,
arbitrary provider fields, and unbounded graphs fail closed during preflight.

The versioned manifest is immutable and content-addressed. It contains source
and destination profile IDs, tenant mapping, resource nodes, dependency edges,
transfer references, checksums, ownership/security scope, unsupported-field
decisions, and source snapshot fingerprint. It excludes credentials, tokens,
private keys, passwords, cookies, and raw provider payloads.

P14.0 establishes only the specified `p14-openstack-cold-migration-v1`
profile. Evidence must progress from portable simulation to process tests and
then a bounded real OpenStack-source/O3K-destination gate proving isolation,
restart/resume, unknown outcomes, rollback, cutover, integrity, cleanup, and
final OpenTofu NO-OP. No live migration, active-active, multi-region, generic
cloud migration, or production SLA claim is made.

## Consequences

Implementation requires durable migration records and integration with O3K
operations and reconciliation. Some source features are rejected rather than
guessed. Source deletion, credential rotation, and provider-specific transfer
remain explicit operator workflows.

## Non-goals

No OpenStack database translation, direct O3K database writes by an importer,
in-place upgrade, live migration, automatic source deletion, password/token/
private-key migration, broad UUID preservation, arbitrary extensions,
active-active, multi-region, generic federation, or production SLA.
