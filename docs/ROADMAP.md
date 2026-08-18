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

The next milestones should therefore increase **tenant-visible cloud product
capability**, not continue infrastructure abstraction for its own sake.

## P9 — O3K Routed Fabric v1

P9 is the highest-value next native product milestone.

User outcome:

> A tenant can boot a real VM on an O3K-owned network, keep its durable fixed
> address, reach an approved external network through controlled egress,
> associate a public/floating address, enforce stateful project-owned network
> policy, survive supported restart/takeover/retry scenarios, and delete the
> resources without O3K-owned network leakage or foreign-state mutation.

Normative architecture is proposed in ADR-0168 and SPEC-0026.

Key direction:

- O3K owns technology-independent network intent;
- Neutron routers/floating IPs/security groups are compatibility projections,
  not internal process topology;
- P9 activates a bounded node-local `o3k-network` execution process because
  routing/NAT/policy/public-address privileges form a distinct failure and
  security domain from libvirt compute execution;
- normal P9 traffic has no mandatory central Neutron-like network/gateway node;
- the first dataplane uses conservative Linux routing + nftables/conntrack;
- the canonical domain and per-node plan do not depend on nftables, eBPF, OVN,
  VXLAN/Geneve, WireGuard, or BGP;
- P9 is IPv4/L3-first and may require non-overlapping prefixes in its default
  routed address realm, but neither restriction becomes a permanent O3K
  architecture invariant;
- future eBPF, OVN/OVS/EVPN/overlay and routed-fabric providers remain possible
  behind explicit capabilities without redefining tenant resource identity.

P9 does **not** mean broad Neutron parity, custom eBPF dataplane work, OVN as a
required dependency, tenant overlays, SR-IOV, trunks, P11 multi-host fabric, or
new platform services.

## P10 — Native persistent storage

After the first routed-network product slice is stable:

- canonical O3K Volume state independent of Cinder wire models;
- selected Cinder compatibility projection;
- typed storage execution through `o3k-storage`;
- local LVM as the first reference backend;
- attach/detach with durable operation/unknown-outcome recovery;
- snapshots after the base lifecycle is proven;
- optional Ceph RBD provider after provider conformance exists;
- secret-safe connection information and strict ownership cleanup.

External-hosted Cinder remains a separate service-testbed profile and is not the
native O3K storage implementation. Boot-from-volume remains a later verified
profile rather than a prerequisite for the first persistent-volume milestone.

## P11 — Small multi-hypervisor edge cloud

Turn the now-useful compute + P9 networking + P10 storage model into the first
multi-host edge profile for approximately 10–20 hypervisors.

Required product work includes:

- multi-host capability inventory, capacity and scheduling;
- drain/failure/reconnect/resync behavior;
- host-aware network/storage placement;
- cross-host network realization using an explicitly accepted fabric provider;
- failure-safe replay/fencing and no duplicate provider mutation;
- upgrade/backup/restore/diagnostics across the supported host count;
- performance, latency, resource and cleanup evidence.

Candidate future network fabric mechanisms include host-level WireGuard for a
small encrypted routed edge fabric and BGP route advertisement where the
physical network supports it. EVPN/VXLAN/Geneve/OVN may be added for profiles
that genuinely require regional L2 adjacency, overlapping address realms,
trunks or legacy network semantics. None is a P9 prerequisite.

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
- P11 edge/multi-host claims remain evidence-bound;
- P12 native API work follows mature domain semantics rather than preceding
  them;
- architecture direction does not replace executable evidence or human review.
