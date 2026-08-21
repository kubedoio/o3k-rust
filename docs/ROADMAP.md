# Roadmap

## Product roadmap model

O3K develops one product:

> **O3K Cloud OS — a lightweight, open, Rust-native Cloud Operating System.**

Primary product targets remain:

1. an OpenStack-compatible native O3K cloud;
2. selected external OpenStack service testbeds;
3. a small edge cloud for roughly 10–20 hypervisors;
4. first-class deployment of the O3K control plane on Kubernetes;
5. a first-class O3K-native API/ecosystem beside the selected OpenStack
   compatibility surface.

OpenStack is a northbound compatibility contract, not O3K's internal topology.
Kubernetes is a deployment substrate, not the Cloud Kernel or tenant-resource
authority. See ADR-0165, ADR-0167, SPEC-0024, and the normative-source map.

## Foundation state

The P0-P8 infrastructure-foundation sequence established the prerequisites for
post-foundation product work: the native TestLab vertical slice, installer and
safe forward-upgrade path, Cloud Kernel IAM/authorization/ownership/service-
registry/audit/quota primitives, PostgreSQL persistence/conformance, Kubernetes
packaging, durable multi-controller work ownership/fencing, and the bounded HA
control-plane profile/evidence.

That history does not turn architecture or evidence from one profile into a
blanket production/HA/SLA claim. Release claims remain profile- and evidence-
bound, especially for external PostgreSQL/shared-storage dependencies and the
exact tested Kubernetes topology.

The post-foundation milestones increase **tenant-visible cloud product
capability**, not infrastructure abstraction for its own sake.

## P9 — O3K Routed Fabric v1 — completed profile

P9 established the native tenant networking foundation:

- technology-independent AddressRealm/endpoint/route/public-address/policy
  intent;
- a bounded node-local `o3k-network` execution boundary;
- conservative Linux routing + nftables/conntrack realization;
- controlled egress/SNAT and bounded public/floating-address behavior;
- stateful NetworkPolicy/security-group projection;
- real guest packet-path, restart/replay, cleanup, and foreign-state evidence.

ADR-0168/SPEC-0026 remain the normative architecture. Their design intentionally
keeps nftables, eBPF, OVN, VXLAN/Geneve, WireGuard, and BGP below the canonical
Network domain.

P9 did not claim regional L2, broad Neutron parity, custom eBPF, OVN, SR-IOV,
trunks, or multi-host fabric behavior.

## P10 — Native persistent storage — completed profile

P10 established native O3K-owned persistent storage:

- canonical Volume/VolumeAttachment/Snapshot state independent of Cinder wire
  models;
- host/backend-scoped `o3k-storage` execution;
- LVM thin-pool reference provider with real guest persistence evidence;
- durable attachment/recovery/unknown-outcome semantics;
- crash-consistent snapshot semantics;
- mandatory Ceph RBD provider proof using the same canonical lifecycle;
- secret-safe connection information and strict ownership cleanup.

ADR-0169/SPEC-0027 are normative. External-hosted Cinder remains a separate
service-testbed profile, not native Volume authority. Boot-from-volume,
multi-attach, migration, backups, replication/mirroring, CephFS/NFS, and KMS
remain later profiles unless separately accepted and proven.

## P11 — Small multi-hypervisor edge cloud — completed profile

P11 turned the proven compute + P9 network + P10 storage model into the first real
multi-hypervisor edge profile for roughly 10–20 hypervisors. Run 50 (commit
3bcf814) passed the complete real-host evidence gate on three independent KVM
hosts with overlapping AddressRealm CIDRs.

ADR-0171/SPEC-0029 plus `contracts/edge-fabric-realm-overlay.md` are the accepted
v2 architecture authority. ADR-0170/SPEC-0028 are superseded.

The confirmed P11 support profile is described below.

### Accepted P11 v2 user outcome

> Independent tenants can use identical private CIDRs in separate AddressRealms,
> place real VMs from each realm on different eligible hypervisors, use normal
> ARP/local Ethernet for same-host peers, resolve remote peers locally without
> cross-host ARP flooding, carry AddressRealm identity through a bounded Geneve
> encapsulation layer over an authenticated/encrypted WireGuard host transport,
> preserve stateful policy and public/FIP behavior, use LVM/RBD according to
> storage locality, survive supported drain/restart/disconnect/reconnect
> scenarios, and delete the environment without duplicate resources, realm
> misdelivery, owned leaks, or foreign-state mutation.

### P11 proven network topology

- one VM-facing host-local Linux bridge per active AddressRealm on a host,
  preserving the proven libvirt/TAP path;
- one routed Linux network namespace per active AddressRealm for gateway,
  proxy-neighbor, routed policy/NAT and fabric attachment;
- same-host/same-realm endpoints use normal ARP and their real endpoint MACs;
- local bridge/TAP traffic remains subject to O3K anti-spoofing and canonical
  NetworkPolicy so local L2 cannot bypass security;
- a control-plane-derived distributed realm endpoint directory identifies
  current local/remote endpoints from accepted endpoint placement;
- remote same-realm ARP is answered locally using a deterministic AddressRealm
  proxy MAC; tenant ARP is not flooded across hypervisors;
- endpoint identity is interpreted as `(AddressRealm, IP)`, not globally by bare
  tenant IP;
- each active AddressRealm receives a durable provider-native Geneve VNI mapping
  in the selected fabric domain;
- remote known-unicast traffic is encapsulated with the current realm VNI and
  sent to the accepted target host;
- Geneve carries realm identity but does not create an arbitrary regional L2
  flood domain;
- one shared WireGuard host transport remains per compute host;
- WireGuard peer routing uses unique provider host-fabric transport addresses,
  not tenant endpoint `/32`s, so overlapping customer IPs remain unambiguous;
- WireGuard provides transport security only: AddressRealm and NetworkPolicy
  remain tenant isolation/authorization;
- host WireGuard private keys remain host-local;

- arbitrary cross-host broadcast/multicast/unknown-unicast flooding remains out
  of scope.

### P11 placement and host lifecycle

- reuse existing authenticated agent inventory, Placement, scheduling, durable
  work leases, fencing, agent epochs, and reconciliation;
- new placement must account for host availability/admin state, compute
  capacity, P11 realm/Geneve/WireGuard readiness, and P10 storage placement
  scope;
- host-local LVM constrains placement to its owning eligible host;
- shared Ceph RBD may be used serially from another eligible host only after the
  previous attachment is cleanly terminated or otherwise safely fenced;
- `Draining` excludes a host from new placement and reports resident workloads/
  local-storage/attachment blockers rather than hiding them behind migration;
- loss of host/controller connectivity is uncertainty, not proof of power-off;
  P11 does not blindly evacuate VMs or reactivate an exclusive shared-storage
  writer elsewhere without accepted fencing proof.

### P11 proven evidence summary

Evidence gate run 50 (commit 3bcf814) on three independent nested KVM hosts
(p11h1/p11h2/p11h3) with overlapping CIDRs across two AddressRealms.

**Real functional topology:** three independent KVM/libvirt hypervisors, one
control-plane host, nested-KVM test-lab topology.

**Network:**
- AddressRealm isolation with overlapping `10.0.0.0/24` CIDRs across hosts;
- same-host actual-MAC ARP via realm bridge;
- remote proxy-MAC ARP — O3K endpoint directory resolves without ARP flooding;
- Geneve VNI realm identity — VNI 101 (realm A), VNI 102 (realm B);
- WireGuard encrypted host transport — zero cleartext tenant packets observed;
- cross-realm isolation — A2 ping to `10.0.0.10` does not appear on B1 tap;
- NetworkPolicy allow/deny in both realms;
- FIP/public path — realm-scoped public bindings verified;
- MTU — near-boundary traffic verified.

**Storage:**
- LVM locality — host-local VG constrains placement;
- serial Ceph RBD cross-host persistence — attach on host A, write/checksum,
  clean detach, attach on host B, checksum matches.

**Lifecycle:**
- drain — Draining state excludes new placement;
- restart/replay — fabric state rebuilds correctly after restart;
- disconnect/reconnect — WireGuard re-handshake, fabric state recovery;
- controller takeover — fencing and lease semantics;
- fabric interruption/recovery — matrix-tested 25 failure scenarios (see
  `scripts/p11-failure-recovery-matrix.sh`).

**Control-plane scale:** 15 simulated/enrolled hosts via `p11-fake-hosts.sh`
for registration, inventory, scheduling, realm/VNI/directory fanout, reconnect,
and controller concurrency. This is simulated-agent evidence and does not
expand the real 3-hypervisor support claim.

**Cleanup:** post-cleanup owned resources across all categories (domains,
netns, bridges, veths, Geneve, WireGuard, routes, nftables, iptables, LVM,
RBD) = 0. Zero foreign mutations.

> Note: the cleanup inventory proves `owned resources remaining after cleanup =
> 0` and `foreign mutations = 0`. It does not independently count duplicate
> resource detections during execution (e.g. duplicate VNI allocation attempts
> that were rejected before becoming leaks). Those are covered by the failure
> recovery matrix and provider conformance tests, not the post-cleanup
> snapshot.

### P11 non-goals

The accepted P11 successor does not mean:

- arbitrary regional L2 adjacency;
- cross-host ARP/Ethernet/unknown-unicast/multicast flooding;
- VXLAN/EVPN or mandatory OVN/OVS;
- custom eBPF dataplane;
- internal BGP;
- STUN/TURN/relay/NAT traversal;
- live migration or automatic unfenced evacuation;
- storage migration or multi-attach;
- SR-IOV/DPDK;
- multi-region;
- P12 native API/CLI or Terraform/UI work.

Future profiles may replace/accelerate the provider with OVN/EVPN/eBPF/BGP when
the product genuinely requires broader L2 semantics, larger topology, hardware
offload, or external-router integration. Such providers must preserve canonical
AddressRealm/endpoint identity.

## P12 — Native O3K Resource API & Service Framework — implementation active

P12 makes the O3K resource model a first-class product API and proves that the
Cloud Kernel can support a new first-class cloud service without service-specific
business logic being added to the kernel.

The P12 architecture is defined by ADR-0173/ADR-0174 with SPEC-0030/SPEC-0031.
Those sources were accepted for implementation as the P12 architecture (human
approval recorded 2026-08-21). The sections below represent the implementation
status as of the current release.

### P12.1 — Kernel contract groundwork — implemented

- `ResourceEnvelope` / `ResourceMeta` — service-neutral native resource envelope
  in `o3k-kernel` (`crates/o3k-kernel/src/envelope.rs`);
- `Operation` / `OperationState` — service-neutral operation model
  (`crates/o3k-kernel/src/operation.rs`);
- `ServiceManifest` — canonical native service identity, resource types, actions,
  capabilities, and dependencies, separated from OpenStack compatibility
  (`crates/o3k-kernel/src/manifest.rs`);
- `OpenStackCompatibilityProjection` — separate projection for Keystone/OpenStack
  catalog metadata;
- `ManifestRegistry` — validated, atomic registration with namespace ownership,
  duplicate detection, resource/action conflict enforcement, and bounded input
  validation;
- Controller protocol contract (`Controller` trait, `ProtocolVersion`,
  `ControllerSession`, proper lifecycle state machine);
- `seed_core()` migration adapter representing P0-P11 services as manifests.
- **No Database-specific knowledge in kernel**: no `ServiceNamespace::database()`,
  no hard-coded database quota dimensions. Extension services use generic
  namespace construction.

### P12.2 — Native discovery API — implemented (scaffolded)

- `crates/o3k-native-api` — sibling northbound adapter (ADR-0173/SPEC-0030);
- `GET /o3k/v1/services` — registered service discovery from `ManifestRegistry`
  with lifecycle state;
- `GET /o3k/v1/resource-types` — resource-type discovery from `ManifestRegistry`;
- `GET /o3k/v1/identity/me` — **stub only**; real IAM wiring is P12.2 follow-up;
- wired into `o3kd` alongside the existing OpenStack API router at `/o3k/v1/...`;
- **Not implemented**: native identity tokens, authentication/authorization,
  representative read-only resources (compute:server, volume:volume, etc.),
  service-neutral Operation exposure, pagination, error envelope.

### P12.3 — Native CLI — implemented (scaffolded)

- `bins/o3k` updated to use `clap` derive for all subcommands;
- existing `doctor`, `version`, `upgrade`, `rollback` commands preserved;
- native API commands: `service list/show`, `resource-type list`;
- `resource list/show` — **command structure exists but server dispatch does not**;
  these are nonfunctional until generic resource routes are implemented on the
  server side;
- **Not implemented**: generic resource create/delete, stable JSON output.

### P12.4 — Protocol adapter convergence — implemented

- `o3k-api` (`AppState`) extended with native API state via `FromRef`;
- native and OpenStack API routes share the same `AppState` composition root
  at `/o3k/v1/...`.

### P12.5 — Controller contract and service boundary — scaffolded

- `Controller` trait in `o3k-kernel` (`crates/o3k-kernel/src/controller.rs`);
- `ProtocolVersion`, `ReconcileOutcome`, `DelegationContext`, `ControllerSession`,
  `ControllerRegistration`, `ControllerState` lifecycle (Declared→Ready);
- `ManifestRegistry` extended with controller registration, health tracking,
  session generation fencing, and activation handshake;
- **Not implemented**: language-neutral external controller transport (gRPC/
  protobuf/mTLS is the ADR-0174 reference direction), secure delegation
  enforcement, service SDK crate. The Rust `Controller` trait is an in-process
  contract only.

### P12.6 — Extension conformance service — scaffolded

- `crates/o3k-database-example` — minimal non-production conformance service;
- namespace `database`, resource type `database:instance`, actions
  `database:CreateInstance`/`ReadInstance`/`DeleteInstance`;
- proves manifest registration and `Controller` trait implementation without
  Database-specific business logic in `o3k-kernel`;
- **Not implemented**: real resource composition (compute:server + network:
  endpoint + volume:volume), bounded delegation, durable operations,
  compensation, audit correlation, cleanup evidence.

### P12.7 — Compatibility and evidence — pending

- native/OpenStack authority convergence tests require a running `o3kd`
  instance with both adapters configured;
- existing OpenStack compatibility tests confirmed no regression (all existing
  tests pass);
- security evidence matrix (cross-project, IDOR, delegation, cursor, etc.) not
  yet implemented.

### P12 non-goals (confirmed)

P12 explicitly does **not** require Terraform/public language SDKs, UI,
WebSocket/event streaming, production DBaaS/DNS/LB/AI/Kubernetes services,
multi-region, dynamic Rust `.so` plugins, or provider/dataplane redesign.
P12 completion requires executable evidence for both native API correctness and
service extensibility. Endpoint count alone is not a completion metric.

## P13+ — richer cloud platform and ecosystem

Possible later tracks include:

- richer IAM/organization/federation capabilities;
- Terraform provider, public SDKs and UI around the native API;
- eBPF network dataplane/observability provider where justified;
- richer Neutron compatibility profiles;
- production load balancing, DNS, object storage, secrets, managed database,
  Kubernetes, AI/ML, or other first-class services built on the P12 framework;
- delegated/federated cloud connectors with explicit authority models;
- service packaging/installation/upgrade ecosystem once its safety/evidence
  contracts are separately proven;
- scale, performance, security and production-certification campaigns.

New services must reuse Cloud Kernel IAM, authorization, ownership, quota,
operations, audit/events and service registration rather than constructing a
parallel cloud framework.

## Continuing compatibility and service-testbed tracks

Selected OpenStack service-testbed and compatibility work may proceed when it
serves a declared user journey, but it must not silently displace the native
product sequence above. External-hosted services retain their own databases,
message buses, processes, backends, upgrades and operational authority.

Route/endpoint count is not a roadmap metric. Compatibility expands because an
accepted product workflow needs it and the corresponding operation-level
evidence exists.

## Roadmap governance

- accepted ADRs/specs/contracts remain authoritative over this summary;
- OpenStack service names do not mandate process boundaries;
- Kubernetes APIs/CRDs do not become Cloud Kernel or canonical tenant state;
- host-local privileged execution stays outside the Kubernetes control plane by
  default;
- P9 networking must not become an excuse for a generic infrastructure
  abstraction program;
- P10 storage must not conflate external Cinder with native Volume authority;
- P11 fabric implementation must not turn WireGuard or Geneve provider IDs into
  tenant identity, silently add regional L2 flooding, or claim a larger real
  topology than was tested;
- do not continue privileged successor fabric implementation from stale
  ADR-0170-era prompt text;
- P12 native API/service-framework implementation must not begin from proposed
  contracts as if they were accepted; human architecture/security approval is
  required first;
- P12 must not hard-code a new service into the Cloud Kernel merely to satisfy
  the conformance example;
- architecture direction does not replace executable evidence or human review.
