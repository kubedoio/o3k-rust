# Normative source ownership

This file identifies the authoritative documents for O3K architecture and
product rules. Summaries explain these decisions but do not override them.

## Authority map

| Subject | Normative source | Summary-only documents |
|---|---|---|
| Cloud OS identity, Cloud Kernel, OpenStack compatibility, provider/delegated-cloud authority | `docs/adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md` | `README.md`, `docs/ARCHITECTURE.md`, `docs/ROADMAP.md` |
| O3K IAM and Keystone compatibility | `docs/adr/ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md` and `docs/specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md` | `README.md`, `docs/ARCHITECTURE.md` |
| Kubernetes-native control-plane deployment and PostgreSQL requirement for HA Kubernetes | `docs/adr/ADR-0167-kubernetes-native-control-plane-deployment.md` | `README.md`, `docs/ROADMAP.md`, `docs/ARCHITECTURE.md` |
| P9 O3K network intent, Routed Fabric, and node-local network execution | `docs/adr/ADR-0168-o3k-routed-fabric-and-network-execution.md` and `docs/specs/SPEC-0026-o3k-routed-fabric-v1.md` | `docs/ROADMAP.md`, `docs/ARCHITECTURE.md`, `docs/PRODUCT_REQUIREMENTS.md` |
| P11 currently accepted v1 multi-host fabric | `docs/adr/ADR-0170-namespaced-routed-edge-fabric.md`, `docs/specs/SPEC-0028-namespaced-routed-edge-fabric-v1.md`, and `contracts/p11-edge-fabric.md` | `docs/ROADMAP.md` |
| P11 proposed overlapping-AddressRealm successor | `docs/adr/ADR-0171-addressrealm-encapsulated-edge-fabric.md`, `docs/specs/SPEC-0029-addressrealm-encapsulated-edge-fabric-v2.md`, and `contracts/p11-realm-overlay-fabric.md` (**Proposed; not active until human acceptance**) | `docs/ROADMAP.md`, `docs/P11_REALM_OVERLAY_IMPLEMENTATION_PROMPT.md` |
| Deployment/evidence profiles and claim posture | `docs/adr/ADR-0163-product-profiles-and-deployment-posture.md`, `docs/specs/SPEC-0024-product-profiles-and-claims.md`, and `compatibility/product-profiles.yaml` | `README.md`, `docs/ROADMAP.md` |
| Service topology, persistence authority, process/crate extraction | `docs/adr/ADR-0160-service-topology-and-execution-boundaries.md`, `docs/specs/SPEC-0025-rust-rewrite-and-architecture-convergence.md`, and `contracts/core-architecture-boundaries.toml` | `README.md`, `docs/ARCHITECTURE.md` |
| Cross-service workflows and compensation | `docs/specs/SPEC-0021-cross-service-workflows-and-compensation.md` | `docs/ARCHITECTURE.md`, `docs/TEST_STRATEGY.md` |
| OpenStack compatibility and evidence gates | `docs/specs/SPEC-0022-service-api-baseline-and-evidence-gates.md` | `README.md`, `docs/ROADMAP.md` |
| External OpenStack service-under-test profile | `docs/specs/SPEC-0023-external-cinder-service-under-test.md` | `README.md`, `docs/ARCHITECTURE.md`, `docs/PROJECT_CHARTER.md`, `docs/ROADMAP.md` |
| Execution authority and protocol invariants | `contracts/execution-boundaries.md` | `docs/ARCHITECTURE.md`, `AGENTS.md` |
| Native persistent Volume/Attachment/Snapshot authority and storage execution | `docs/adr/ADR-0169-native-persistent-storage-and-o3k-storage-boundary.md` and `docs/specs/SPEC-0027-native-persistent-storage-v1.md` | `docs/ROADMAP.md`, `docs/PRODUCT_REQUIREMENTS.md` |

## Core rules

- O3K owns public identity, ownership, desired state, scheduling, operations,
  reconciliation, and provider mappings for O3K-owned resources.
- OpenStack service names are compatibility concepts, not mandatory internal
  process boundaries.
- O3K IAM is canonical; Keystone is a compatibility projection.
- ADR-0168/SPEC-0026 establish that O3K Network owns technology-independent
  connectivity intent, with Neutron objects as compatibility projections and
  nftables/eBPF/OVN/overlay mechanisms as execution-provider concerns.
- ADR-0168 activates `o3k-network` only as a bounded node-local execution
  authority; it does not gain tenant authorization, scheduling, public-ID
  allocation, or an independent cloud desired-state database.
- ADR-0170/SPEC-0028 are currently accepted P11 authority. They introduced the
  realm bridge/netns, distributed endpoint directory, proxy-MAC remote neighbor
  resolution, and one shared WireGuard host fabric.
- ADR-0171/SPEC-0029 are a **proposed successor**. They do not supersede the
  accepted v1 authority unless/until explicit human acceptance is recorded.
- While that successor is under review, do not extend privileged P11 fabric
  implementation from stale v1 prompt text. PR #703's portable realm-scoped
  endpoint-directory/planner semantics may be retained where compatible.
- The proposed successor interprets cross-host tenant addresses as
  `(AddressRealm, IP)`, uses a durable provider mapping from AddressRealm to a
  Geneve VNI, and makes WireGuard route only unique provider host-fabric
  transport addresses rather than tenant endpoint `/32`s.
- Geneve in the proposed successor carries realm identity for known-unicast
  traffic; it does not create an implicit regional ARP/broadcast/unknown-unicast
  flooding domain.
- In both P11 designs, same-host/same-realm local L2 is allowed only when
  anti-spoofing and canonical NetworkPolicy are enforced on the TAP/bridge
  path. Packet learning/FDB/ARP observations never become endpoint authority.
- WireGuard is authenticated encrypted host transport, never tenant isolation or
  authorization. Private fabric keys stay host-local.
- Kubernetes is a first-class **control-plane deployment target**, not the Cloud
  Kernel and not the tenant-resource database.
- The Cloud Kernel/domain crates remain independent from Kubernetes APIs.
- PostgreSQL is required before an HA/cloud-native Kubernetes support claim.
  SQLite remains the minimal single-controller/TestLab store.
- Host-local compute/network/storage execution remains outside Kubernetes by
  default.
- Kubernetes CRDs do not become the canonical O3K server/network/volume/
  operation store.
- Existing OpenStack, vSphere, Proxmox, KubeVirt, or public clouds require a
  delegated/federated authority model rather than a libvirt-like provider.

## Claim discipline

Architecture is not release evidence. PostgreSQL/Kubernetes HA, routed
networking, native storage, P11 multi-host networking, broad federation,
complete parity, and fixed footprint/scale claims remain limited to the exact
implementation and evidence profiles that passed their gates.

The current compatibility manifest remains authoritative for which OpenStack
operations are actually advertised. Proposed/accepted P11 fabric architecture
does not itself expand Neutron, Nova, or Cinder compatibility.

The proposed overlapping-AddressRealm P11 successor must not be claimed until a
real multi-host gate proves two independent realms/projects using the same CIDR
and overlapping endpoint IPs with zero cross-realm misdelivery.

P11 must state the exact tested real-hypervisor topology. Simulated agents may
supplement scale/concurrency evidence but do not expand a real-hypervisor
support claim.

The current ephemeral-root libvirt TestLab remains the first-alpha blocking path
unless a later human-approved decision explicitly replans it.
