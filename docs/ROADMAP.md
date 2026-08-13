# Roadmap

## Product roadmap model

O3K develops one product:

> **O3K Cloud OS — a lightweight, open, Rust-native Cloud Operating System.**

Primary product targets are:

1. OpenStack-compatible native TestLab/cloud;
2. selected external OpenStack service testbeds;
3. a small edge cloud for roughly 10–20 hypervisors;
4. **first-class deployment of the O3K control plane on Kubernetes.**

Kubernetes is a deployment substrate target, not a separate cloud-authority
model. See ADR-0165, ADR-0167, and SPEC-0024.

## Non-negotiable sequencing rule

The current release remains `v0.2.0-alpha.1`, a Rust-native
OpenStack-compatible libvirt TestLab alpha. Kubernetes packaging, PostgreSQL,
multi-controller HA, native volumes, and broader Cloud Kernel work do not expand
that release gate without a separate human-approved replan.

## Track A — Finish the native libvirt alpha

Prove the selected Keystone/Image/Network/Placement/Compute workflow through
`o3k-compute` and real libvirt/QEMU/KVM with restart, reconciliation, cleanup,
foreign-state protection, evidence, and release packaging.

## Track B — Cloud Kernel convergence

After the alpha:

- converge current auth on O3K IAM / `AuthContext`;
- introduce shared Action/Resource/Ownership authorization primitives;
- make the service registry and Keystone catalog projection explicit;
- converge resource/operation/audit semantics across current domains;
- strengthen architecture fitness functions.

## Track C — PostgreSQL persistence

PostgreSQL is a main production-oriented target and the required persistence
foundation before an HA Kubernetes control-plane claim.

Required work:

- real `o3k-store` PostgreSQL adapter;
- migrations and SQLite/PostgreSQL conformance suite;
- transaction/isolation decisions;
- upgrade/rollback and backup/restore;
- reconnect/failure behavior;
- no authoritative controller state dependent on a local filesystem.

Shared SQLite or distributed-filesystem workarounds are not an HA design.

## Track D — Kubernetes-native control plane

Kubernetes deployability is a **main O3K product target**, not community
packaging added later.

### D1. Single-controller cloud-native packaging

- OCI image for `o3kd`;
- explicit config and credential adapters suitable for container deployment;
- `/healthz` and `/readyz` wired to Kubernetes probes;
- graceful termination/readiness drain contract;
- small Helm chart or equivalent rendered manifests;
- one-controller Kubernetes smoke/e2e evidence;
- pod-local filesystem classified as cache/scratch only for authoritative state.

This phase may remain non-HA and must be advertised honestly.

### D2. Multi-controller foundation

After PostgreSQL exists:

- multiple `o3kd` API replicas;
- durable background-work ownership;
- controller generation/fencing or equivalent stale-owner protection;
- safe scheduler/reconciler/compensator ownership transfer;
- DB-backed coordination as the preferred first portable implementation;
- optional Kubernetes Lease adapter only where useful, never as the sole cloud
  correctness mechanism.

### D3. Kubernetes HA evidence

Before a Kubernetes HA support claim:

- rolling update;
- pod deletion and abrupt process loss;
- node drain / disruption behavior;
- PostgreSQL reconnect/failover;
- background-work ownership transfer;
- no duplicate provider mutation;
- no loss of durable desired state or operations;
- documented upgrade/rollback and operational recovery.

### D4. Operator only when justified

Helm comes first. A dedicated O3K Operator is a later decision only if
O3K-specific lifecycle automation such as coordinated migrations, certificate
rotation, backup/restore, or profile management clearly needs one.

## Track E — Small edge cloud

- approximately 10–20 hypervisors;
- multi-host inventory/capacity/scheduling;
- host enrollment, mTLS, epochs, heartbeat, reconnect/resync;
- failure-safe replay/fencing;
- host-aware networking and cleanup;
- backup/restore/upgrade/rollback/diagnostics;
- database profile matching the claimed concurrency/availability.

Kubernetes may host this profile's control plane, but hypervisor execution stays
host-local by default.

## Track F — Native Volume / Cinder compatibility

After the alpha and enough Cloud Kernel convergence:

- canonical O3K Volume state;
- selected Cinder compatibility;
- typed storage provider and `o3k-storage`;
- local LVM reference backend;
- optional Ceph RBD;
- attachment recovery/cleanup.

## Track G — First genuinely new O3K-native service

Choose the first new service for product value and architectural learning. It
must reuse Cloud Kernel IAM, authorization, ownership, operations, audit, and
service registration rather than build a parallel framework.

## Track H — Delegated/federated clouds

Existing OpenStack, vSphere/vCenter, Proxmox, KubeVirt, or public clouds require
an explicit authority model. Do not create a lowest-common-denominator generic
provider abstraction.

KubeVirt remains in this category even when O3K itself runs on Kubernetes.

## Roadmap governance

- OpenStack service names do not mandate process boundaries;
- Kubernetes APIs do not become Cloud Kernel/domain dependencies;
- Kubernetes CRDs do not become the canonical tenant-resource database;
- host execution remains outside Kubernetes by default;
- PostgreSQL, Kubernetes HA, edge-production, native Cinder, federation, and
  future service breadth remain evidence-gated;
- architecture direction does not replace release evidence.
