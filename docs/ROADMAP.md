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

## P11 — Namespaced Routed Edge Fabric v1 — accepted implementation milestone

Turn the proven compute + P9 network + P10 storage model into the first real
multi-hypervisor edge profile for roughly 10–20 hypervisors.

The accepted normative architecture is ADR-0170/SPEC-0028 plus
`contracts/p11-edge-fabric.md`. Runtime implementation is authorized, but no
runtime, scale, or support claim exists until the required evidence gates pass.

### P11 user outcome

> A tenant can place VMs from one AddressRealm/subnet on different eligible
> hypervisors, use normal ARP/local Ethernet for same-host peers, reach remote
> same-realm endpoints without cross-host ARP flooding through an O3K-derived
> endpoint directory and endpoint `/32` routes, preserve stateful policy and
> public/FIP behavior, use LVM/RBD according to storage locality, survive
> supported drain/restart/disconnect/reconnect scenarios, and delete the
> environment without duplicate resources, owned leaks, or foreign-state
> mutation.

### P11 reference network topology

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
  proxy MAC; ARP is not flooded across hypervisors;
- remote endpoint reachability follows accepted endpoint location using host
  routes, normally IPv4 `/32`, instead of tying a tenant subnet to one host;
- one shared routed host fabric per compute host, with WireGuard as the
  reference encrypted transport;
- WireGuard provides transport security only: AddressRealm and NetworkPolicy
  remain tenant isolation/authorization;
- host WireGuard private keys remain host-local; only bounded public fabric
  identity/generation is distributed;
- P11 v1 keeps non-overlapping AddressRealm prefixes across the shared routed
  fabric and does not claim regional L2.

### P11 placement and host lifecycle

- reuse existing authenticated agent inventory, Placement, scheduling, durable
  work leases, fencing, agent epochs, and reconciliation;
- new placement must account for host availability/admin state, compute
  capacity, P11 network/fabric readiness, and P10 storage placement scope;
- host-local LVM constrains placement to its owning eligible host;
- shared Ceph RBD may be used serially from another eligible host only after the
  previous attachment is cleanly terminated or otherwise safely fenced;
- `Draining` excludes a host from new placement and reports resident workloads/
  local-storage/attachment blockers rather than hiding them behind migration;
- loss of host/controller connectivity is uncertainty, not proof of power-off;
  P11 does not blindly evacuate VMs or reactivate an exclusive shared-storage
  writer elsewhere without accepted fencing proof.

### P11 evidence posture

- core functional evidence uses at least three independent real KVM/libvirt
  compute hosts unless the accepted SPEC selects a stronger minimum;
- real tests prove local actual-MAC ARP, remote proxy-MAC ARP, no cross-host ARP
  flood dependency, WireGuard-encrypted packet path, local/cross-host policy
  allow/deny, public/FIP behavior, MTU, LVM locality, serial RBD checksum
  persistence, drain/reconnect/failure recovery, and independent cleanup;
- separate target-count evidence exercises approximately the roadmap host count
  for registration, inventory, scheduling, directory/fabric-plan fanout,
  reconnect, and controller concurrency;
- simulated agents may supplement scale evidence but do not expand the real
  hypervisor support claim.

### P11 non-goals

P11 v1 does not mean:

- cross-host overlapping CIDRs;
- regional L2 adjacency or ARP/Ethernet flooding;
- VXLAN/Geneve/EVPN or OVN/OVS;
- custom eBPF dataplane;
- internal BGP;
- STUN/TURN/relay/NAT traversal;
- live migration or automatic unfenced evacuation;
- storage migration or multi-attach;
- SR-IOV/DPDK;
- multi-region;
- P12 native API/CLI or Terraform/UI work.

Future profiles may add VRF/encapsulation/OVN/EVPN/eBPF/BGP where the product
actually requires regional L2, overlapping realms, larger topology, hardware
offload, or external-router integration.

## P12 — Native O3K API and CLI

After the primary IaaS domains have mature native semantics, make the O3K
resource model the first-class product API rather than forcing every capability
through historical OpenStack shapes.

Goals:

- native O3K API contracts for the stable Cloud Kernel resource model;
- native CLI built around O3K resource/workflow semantics;
- OpenStack remains a selected northbound compatibility adapter rather than the
  primary internal/product model;
- no loss of verified OpenStack compatibility merely because a native API
  exists;
- API design uses the proven Compute/Network/Volume/IAM semantics rather than
  freezing an incomplete resource model too early.

## P13+ — richer cloud platform and ecosystem

Possible later tracks include:

- richer IAM/organization/federation capabilities;
- Terraform provider, SDKs and UI around the native API;
- eBPF network dataplane/observability provider where justified;
- richer Neutron compatibility profiles;
- load balancing, DNS, object storage, secrets or other first-class services;
- delegated/federated cloud connectors with explicit authority models;
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
- P11 fabric implementation must not turn WireGuard into tenant identity,
  silently add regional L2, or claim a larger real topology than was tested;
- P12 native API work follows mature domain semantics rather than preceding
  them;
- architecture direction does not replace executable evidence or human review.
