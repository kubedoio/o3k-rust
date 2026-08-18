# Normative source ownership

This file identifies the authoritative documents for O3K architecture and
product rules. Summaries explain these decisions but do not override them.

## Authority map

| Subject | Normative source | Summary-only documents |
|---|---|---|
| Cloud OS identity, Cloud Kernel, OpenStack compatibility, provider/delegated-cloud authority | `docs/adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md` | `README.md`, `docs/ARCHITECTURE.md`, `docs/ROADMAP.md` |
| O3K IAM and Keystone compatibility | `docs/adr/ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md` and `docs/specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md` | `README.md`, `docs/ARCHITECTURE.md` |
| Kubernetes-native control-plane deployment and PostgreSQL requirement for HA Kubernetes | `docs/adr/ADR-0167-kubernetes-native-control-plane-deployment.md` | `README.md`, `docs/ROADMAP.md`, `docs/ARCHITECTURE.md` |
| Proposed P9 O3K network intent, Routed Fabric, and node-local network execution | `docs/adr/ADR-0168-o3k-routed-fabric-and-network-execution.md` and `docs/specs/SPEC-0026-o3k-routed-fabric-v1.md` (not active until human-accepted) | `docs/ROADMAP.md`, `docs/ARCHITECTURE.md`, `docs/PRODUCT_REQUIREMENTS.md` |
| Deployment/evidence profiles and claim posture | `docs/adr/ADR-0163-product-profiles-and-deployment-posture.md`, `docs/specs/SPEC-0024-product-profiles-and-claims.md`, and `compatibility/product-profiles.yaml` | `README.md`, `docs/ROADMAP.md` |
| Service topology, persistence authority, process/crate extraction | `docs/adr/ADR-0160-service-topology-and-execution-boundaries.md`, `docs/specs/SPEC-0025-rust-rewrite-and-architecture-convergence.md`, and `contracts/core-architecture-boundaries.toml` | `README.md`, `docs/ARCHITECTURE.md` |
| Cross-service workflows and compensation | `docs/specs/SPEC-0021-cross-service-workflows-and-compensation.md` | `docs/ARCHITECTURE.md`, `docs/TEST_STRATEGY.md` |
| OpenStack compatibility and evidence gates | `docs/specs/SPEC-0022-service-api-baseline-and-evidence-gates.md` | `README.md`, `docs/ROADMAP.md` |
| External OpenStack service-under-test profile | `docs/specs/SPEC-0023-external-cinder-service-under-test.md` | `README.md`, `docs/ARCHITECTURE.md`, `docs/PROJECT_CHARTER.md`, `docs/ROADMAP.md` |
| Execution authority and protocol invariants | `contracts/execution-boundaries.md` | `docs/ARCHITECTURE.md`, `AGENTS.md` |

## Core rules

- O3K owns public identity, ownership, desired state, scheduling, operations,
  reconciliation, and provider mappings for O3K-owned resources.
- OpenStack service names are compatibility concepts, not mandatory internal
  process boundaries.
- O3K IAM is canonical; Keystone is a compatibility projection.
- ADR-0168/SPEC-0026 propose that O3K Network own technology-independent
  connectivity intent, with Neutron objects as compatibility projections and
  nftables/eBPF/OVN/overlay mechanisms as execution-provider concerns. This P9
  rule is not active until the proposed architecture is human-accepted.
- ADR-0168 proposes activating `o3k-network` only as a bounded node-local
  execution authority; it would not gain tenant authorization, scheduling,
  public-ID allocation, or an independent cloud desired-state database.
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

Architecture is not release evidence. PostgreSQL, Kubernetes HA, production HA,
native volumes, P9 routed networking, broad federation, complete parity, and
fixed footprint claims remain unavailable until their implementation and
evidence gates pass.

The current compatibility manifest remains authoritative for which Neutron
operations are actually advertised; proposed P9 router/floating-IP/security-
group architecture does not itself make those operations supported.

The current ephemeral-root libvirt TestLab remains the first-alpha blocking path
unless a later human-approved decision explicitly replans it.
