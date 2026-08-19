# ADR-0169 — Native persistent storage and the `o3k-storage` boundary

Status: Proposed
Date: 2026-08-19
Decision-accepted: pending human architecture/security review
Human-approval: pending
Supersedes: none
Superseded-by: none
Affected-services: volume, compute, storage, placement, governance

## Context

O3K currently has a bounded external-Cinder attachment adapter and a durable
Nova attachment record. Those records do not make Cinder O3K-owned, and they do
not define a native persistent-volume authority. The next native product slice
requires durable volumes that survive VM deletion and control-plane/provider
restart while preserving the authority and security rules in ADR-0160,
ADR-0165, SPEC-0021, and `contracts/execution-boundaries.md`.

ADR-0160 requires a separate accepted ADR before activating `o3k-storage`.
This proposal is that decision. It is intentionally a gate: implementation of
the native storage process and profile must not begin until a human maintainer
accepts this ADR and SPEC-0027 in writing.

## Decision proposed for acceptance

### 1. Native O3K Volume is a Cloud Kernel service domain

The canonical domain owns typed, durable:

- `Volume`: public ID, project/security scope, size, type, desired/observed
  lifecycle, selected backend/failure domain, provider mapping, generation,
  operation identity, and audit identity;
- `VolumeAttachment`: public ID, volume/server/project IDs, host, device/mode,
  desired/observed lifecycle, storage and compute phases, generation, and
  compensation state;
- `Snapshot`: public ID, source-volume identity/generation, project scope,
  immutable source/content semantics, desired/observed lifecycle, provider
  mapping, and documented consistency semantics;
- backend capability and placement state: typed capacity, allocation unit,
  supported operations, failure domain, availability, and evidence state.

The domain does not depend on Cinder JSON, SQLx, protobuf, libvirt, LVM,
Ceph, paths, device names, or connection blobs. Cinder is a northbound
compatibility projection only.

### 2. Authority and process boundary

`o3kd` remains authoritative for public IDs, project ownership,
authentication/authorization, quotas, desired state, backend selection,
operation identity, scheduling, reconciliation, compatibility projection, and
audit identity. `o3k-storage` is only a mutually authenticated, node-local
executor/observer for bounded backend mutations and observations.

`o3k-storage` may not authorize tenants, allocate public IDs, select a project,
change desired state, choose an arbitrary backend, or mutate a resource without
an exact O3K ownership marker. It uses the common command envelope, agent epoch,
controller fence, generation, deadline, deterministic idempotency identity,
canonical fingerprint, durable local journal, and observe-before-retry rules.

### 3. Provider scope

The first provider is `LvmStorageProvider` against one explicitly configured,
dedicated volume group. It must reject ambiguous or foreign logical volumes;
an LV name, path, or matching size alone is never ownership evidence. Each owned
backend object carries a verifiable O3K ownership identity and immutable
resource binding.

After LVM passes provider conformance and the full real-guest storage gate, the
same domain/provider contract may be implemented by `CephRbdStorageProvider`.
Ceph credentials, keyrings, monitor addresses, and connection details remain
typed secret-bearing adapter data and never enter public resources, ordinary
logs/events, or evidence.

### 4. Attachment authority and workflow

Attachment is a first-class O3K workflow coordinating storage and compute:

```text
validated -> attachment-intent-persisted -> storage-prepared
-> compute-attached -> device-observed -> attached
```

Detach and compensation reverse the dependency order. A timeout or transport
loss after a possible storage mutation is `unknown`, not failure; observation
precedes retry or compensation. Equivalent command replay returns the durable
outcome. Conflicting fingerprints, stale controller fences, stale agent epochs,
stale generations, and provider-reference mismatches fail closed.

### 5. Snapshot semantics

P10 initially supports snapshots only after the base volume lifecycle is
proven. The selected provider must document whether a snapshot is crash
consistent or application consistent, whether it is point-in-time immutable,
whether it may be created while attached, and its cleanup/retry behavior. No
clone, backup, replication, mirroring, encryption/KMS, or boot-from-volume
claim is implied.

### 6. Compatibility and product claims

P10 adds only the bounded Cinder-compatible operations needed for the proven
journey: volume create/show/list/delete, selected volume type/capability
projection if required, attachment create/update/complete/show/list/delete,
and snapshot create/show/list/delete. Exact methods, fields, versions, auth
actions, deviations, and evidence IDs are frozen in SPEC-0027 before
advertisement.

Native volume claims belong to the native O3K TestLab profile and remain
separate from the external-Cinder service-testbed profile. Boot-from-volume,
multi-attach, live migration, backup, replication, RBD mirroring, CephFS/NFS,
KMS/encryption, CSI, full Cinder parity, P11 multi-host storage, and P12 native
API work are explicit non-goals.

## Security and cleanup gate

The release gate must demonstrate a real guest workflow:

```text
create volume -> boot guest -> attach -> filesystem/write/checksum
-> detach -> supported restart/replay -> reattach -> verify data
-> snapshot and document semantics -> delete VM while detached volume survives
-> reattach/use -> delete snapshot/volume
```

The evidence matrix must independently prove:

```text
owned backend leaks = 0
owned attachment leaks = 0
owned inconsistencies = 0
foreign mutations = 0
```

Evidence is redacted and source-bound. A provider object, XML fragment, fake
provider result, or successful API response alone is insufficient.

## Human acceptance record

Before implementation begins, a human architecture/security reviewer must
record one of:

```text
Decision: ACCEPT ADR-0169 and SPEC-0027 for P10 implementation
Reviewer: <GitHub identity and organization>
Date: <UTC date>
Conditions: <none or explicit bounded conditions>
```

Rejection or requested changes keep this ADR proposed and block activation of
`o3k-storage`.

## Consequences

This adds a real storage authority and a new privileged execution boundary,
with corresponding protocol, migration, provider, recovery, and real-host
evidence work. It does not change the current alpha release claims or make
external Cinder native.

## Rejected alternatives

- Treating external Cinder or `o3k-cinder` as native O3K Volume authority.
- Reusing a libvirt/compute provider abstraction for storage.
- Letting backend paths or LV/RBD names stand in for ownership.
- Returning unrestricted connection information through public APIs or logs.
- Activating a daemon solely because Cinder has a historical service process.

