# SPEC-0027 — Native persistent storage v1

Status: Accepted

Decision-accepted: 2026-08-19
Human-approval: Senol Colak (@senolcolak), Kubedo GmbH, issue comment #5339362254

Related decision: [ADR-0169](../adr/ADR-0169-native-persistent-storage-and-o3k-storage-boundary.md)

Related normative sources:

- [ADR-0160](../adr/ADR-0160-service-topology-and-execution-boundaries.md)
- [ADR-0163](../adr/ADR-0163-product-profiles-and-deployment-posture.md)
- [ADR-0165](../adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [SPEC-0021](SPEC-0021-cross-service-workflows-and-compensation.md)
- [SPEC-0022](SPEC-0022-service-api-baseline-and-evidence-gates.md)
- [SPEC-0024](SPEC-0024-product-profiles-and-claims.md)
- [SPEC-0025](SPEC-0025-rust-rewrite-and-architecture-convergence.md)
- [execution boundary contract](../../contracts/execution-boundaries.md)

## Purpose and gate

This specification freezes the native O3K persistent-volume profile before
implementation. Senol Colak’s human architecture/security approval is recorded
in issue comment #5339362254. The existing external-Cinder profile and Nova
attachment adapter remain separate, and native volume operations remain
unadvertised until their operation-level evidence gates pass.

## Profile and authority

Profile: native O3K TestLab persistent-volume extension, SQLite default, real
libvirt/KVM guest evidence. This extends the native-rust-testlab profile; it
does not alter the first-alpha release gate or the external-hosted Cinder
profile.

Authority mode: `o3k-implemented` for Volume/Attachment/Snapshot domain state;
host- or backend-scoped execution provider for `o3k-storage`; external-hosted
only for the existing Cinder service-testbed. LVM v1 uses host-local placement
and attachment scope. Ceph RBD may use shared cluster/region placement and
attachment scope; the canonical model must not require local-disk topology.

`o3kd` owns canonical state, authorization, quotas, backend selection, durable
operations, reconciliation, and compatibility projection. `o3k-storage` owns
only bounded backend execution and observations on an enrolled host.

## Canonical resources

All IDs are stable typed O3K UUIDs and all resources have a project/security
scope, operation identity, generation, desired state, observed state, and
provider mapping where applicable.

### Volume

Required fields: ID, project, size in bytes, volume type, selected backend and
failure domain, desired/observed status, generation, operation, provider
reference, timestamps, and bounded display metadata. Size is validated before
backend dispatch and cannot be silently changed by a provider.

### VolumeAttachment

Required fields: ID, project, volume ID, server ID, technology-independent
attachment intent and mode, storage phase, compute phase, desired/observed
status, generation, operation, and compensation state. Canonical state must
not contain Linux device paths, `/dev/*`, device-mapper paths, RBD device
names, libvirt target names, or other provider-native device identity. Those
are bounded provider/compute observations only.

Connection information is a typed secret-bearing operational result. It is
never part of a normal resource representation, audit event, log, or CI
artifact. Public responses expose only the selected bounded fields required by
the frozen compatibility operation.

### Snapshot

Required fields: ID, project, source volume ID and source generation, backend
provider reference, desired/observed status, documented consistency semantics,
and operation identity. Snapshot creation must preserve the source volume's
ownership and must not expose provider credentials.

## Operations and failure semantics

Every mutation persists intent and operation phase before side effects. The
command identity is deterministic from operation/resource/action/generation;
the canonical payload fingerprint excludes volatile deadlines. The executor
persists acceptance before mutation and durably records terminal outcomes.

The following cases are mandatory:

- equivalent duplicate delivery replays the original durable outcome;
- conflicting payload with the same identity is rejected;
- stale controller token/fence, agent epoch, and resource generation fail
  closed;
- timeout or connection loss after acceptance becomes unknown outcome;
- unknown outcome is observed by an ownership-safe inspect before retry;
- partial realization converges through reconciliation;
- restart of `o3kd`, `o3k-storage`, libvirt, or LVM control tools does not
  create a duplicate backend object or attachment;
- deletion retains intent until absence is observed and cleans only owned
  resources;
- provider reference changes or ambiguous ownership stop mutation and surface
  an operator-visible failure.

## Provider contract

The typed storage provider contract reports capabilities and bounded
observations, including capacity, allocation unit, volume/snapshot/attachment
support, provider reference, lifecycle, and redacted failure category.

`LvmStorageProvider` is the first/reference implementation and accepts only a
dedicated configured volume group. Ownership is proved by an O3K-managed
metadata marker bound to the canonical volume ID, project scope, generation,
and provider namespace; LV names and paths are hints, never proof. It supports
create, inspect, delete, prepare attachment, terminate attachment, and the
selected snapshot semantics.

The protected LVM gate may provision an isolated, dedicated, disposable VG and
snapshot-capable profile itself, including a loop-backed implementation where
appropriate. It must never adopt or mutate pre-existing VGs/LVs and must prove
independent cleanup. Operator deployments may provide an explicitly configured
dedicated VG. The reference LVM profile is a bounded thin-pool VG, and its
snapshots are crash-consistent; no guest/application quiescing is claimed.

`CephRbdStorageProvider` is required for P10 completion, but may begin only
after LVM passes the complete provider-conformance and real-guest gates. The
same canonical Volume/VolumeAttachment/Snapshot lifecycle must pass against a
real Ceph RBD backend, including create, attach, guest I/O, detach, restart,
snapshot, delete, and foreign-image protection. RBD image names, pool names,
monitor addresses, credentials, and keyrings remain adapter-local. RBD
snapshot consistency and attachment scope must be explicitly documented
before its gate.

## Required public compatibility subset

The implementation must first add an operation record for each advertised
operation with method/path, API version/microversion, auth action/scope,
request/response/error shape, domain transition, dependencies, provider
capability, and evidence IDs. The initial subset is:

- volume create, list, show, delete;
- volume attachment create, update/complete, list, show, delete;
- snapshot create, list, show, delete;
- only the Nova attach/detach projection needed by the real journey.

Volume type and capability discovery are included only if required by the
selected client journey. Boot-from-volume, multi-attach, migration, backup,
replication, mirroring, CephFS/NFS, encryption/KMS, CSI, and full parity are
unsupported and unadvertised.

## Required tests and evidence

Before any release claim:

1. domain, authorization, quota, migration, store-conformance, protocol, and
   secret-redaction tests;
2. stateful fake-provider conformance for all failure windows;
3. real `o3k-storage` process/mTLS/restart/replay tests;
4. LVM component gate with foreign-volume byte/state preservation and
   disposable isolated-VG/thin-pool cleanup evidence;
5. real guest workflow with filesystem creation, unique payload/checksum,
   detach/reattach and supported restarts;
6. snapshot semantics evidence;
7. mandatory Ceph RBD component and real-guest gate after LVM promotion;
8. machine-readable failure matrix with the four zero-count invariants.

The gate must include controller and storage-agent abrupt/graceful restart,
transport interruption after acceptance, duplicate equivalent delivery,
conflicting replay, stale fences/epochs/generations, partial realization,
unknown outcome, interrupted delete, foreign-state preservation, and backend
recovery. A fake or skipped real guest is not full-profile evidence.

## Non-goals

Boot-from-volume, multi-attach, live migration, backups, replication, RBD
mirroring, CephFS/NFS, KMS/encryption, CSI, full Cinder parity, P11
multi-host/edge storage, P12 native API, and external-Cinder replacement.

## Acceptance record

Accepted by Senol Colak (@senolcolak), Kubedo GmbH, on 2026-08-19 via issue
comment #5339362254. The acceptance requires the mandatory Ceph RBD completion
gate and does not waive any provider, security, compatibility, or real-guest
evidence requirement.
