# P9 implementation prompt — O3K Routed Fabric v1

Use the prompt below with a coding LLM after the P9 architecture has received
human acceptance.

```text
Repository: kubedoio/o3k-rust
Program: P9 — O3K Routed Fabric v1 / routable tenant networking and network security
Program tracker: #655

MISSION

Build the first useful O3K tenant network product slice after the P0-P8
infrastructure foundation. The outcome is not "implement Neutron" and not
"build an SDN framework". The outcome is:

A tenant can boot a real VM on an O3K-owned network, keep its durable fixed IP
and MAC, reach an approved external network through controlled node-local
egress/SNAT, associate a public/floating address, enforce project-owned
stateful network policy, survive supported controller/network-executor
restart/takeover/retry scenarios, and delete everything with zero O3K-owned
network leaks and zero foreign-state mutation.

SOURCE OF TRUTH AND HARD GATE

Before changing code:

1. inspect current `main`, open issues, open PRs, compatibility manifests,
   product profiles and current evidence; do not assume this prompt is newer
   than the repository;
2. read `AGENTS.md` and `docs/NORMATIVE_SOURCES.md`;
3. read at minimum:
   - `docs/adr/ADR-0160-service-topology-and-execution-boundaries.md`
   - `docs/adr/ADR-0163-product-profiles-and-deployment-posture.md`
   - `docs/adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md`
   - `docs/adr/ADR-0168-o3k-routed-fabric-and-network-execution.md`
   - `docs/specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md`
   - `docs/specs/SPEC-0021-cross-service-workflows-and-compensation.md`
   - `docs/specs/SPEC-0022-service-api-baseline-and-evidence-gates.md`
   - `docs/specs/SPEC-0024-product-profiles-and-claims.md`
   - `docs/specs/SPEC-0026-o3k-routed-fabric-v1.md`
   - `contracts/execution-boundaries.md`
   - `contracts/core-architecture-boundaries.toml`
   - `compatibility/openstack-targets.yaml`
   - existing accepted network-safety ADRs relevant to the slice;
4. inspect the current `crates/o3k-network`, compute-agent/network realization,
   network repository/schema, public API adapter, reconciliation and test
   harness before proposing changes;
5. inspect #655 and its child issues/PRs before creating new work.

STOP CONDITION:

If ADR-0168 or SPEC-0026 is still `Proposed`, STOP. Do not implement runtime P9
behavior. Report exactly that human architecture approval is still required.
Do not silently promote either document to Accepted. Do not work around this
check by implementing "preparation" abstractions.

If the architecture is Accepted, continue below.

OPERATING MODEL

Work issue-by-issue. #655 is the P9 program tracker, not permission for one
mega-PR. One coherent child issue should normally own one implementation slice,
and one PR should normally close that issue.

Before each slice, record the AGENTS.md required plan:

- issue being solved;
- selected deployment/evidence profile;
- canonical O3K service/domain;
- Neutron compatibility adapter involved, if any;
- authority mode;
- files expected to change;
- contracts/specs affected;
- public OpenStack references/pinned client behavior;
- actions/resources and authorization scope;
- database/execution assumptions;
- workflow phases/compensation;
- required evidence tier;
- tests to add first;
- known uncertainties;
- explicit non-goals.

Do not create micro-issues for acceptance criteria already owned by a coherent
child issue. Do not expand a child issue because an adjacent networking feature
looks easy.

ARCHITECTURE YOU MUST PRESERVE

1. O3K owns technology-independent network intent.

Canonical domain/application state must not depend on:

- nftables/iptables rule syntax or handles;
- eBPF programs/maps;
- OVS/OVN models;
- VXLAN/Geneve/EVPN identifiers;
- WireGuard peer configuration;
- BGP daemon configuration;
- raw `ip`, `nft`, `tc` or shell commands.

Provider-native state is a bounded mapping/observation only.

2. `o3kd` remains authoritative for:

- public O3K IDs;
- project/security ownership;
- IAM/AuthContext and authorization;
- quotas/limits where selected;
- address/public-address allocation intent;
- desired endpoint/route/gateway/NAT/policy state;
- host binding/scheduling intent;
- operations, compensation and reconciliation;
- compatibility behavior/public errors;
- audit/event identity.

3. P9 activates node-local `o3k-network` as a bounded executor.

`o3k-network` owns only:

- host network capabilities/health;
- O3K-owned host-local realization;
- provider observations;
- local journal/ownership evidence required for safe replay/cleanup.

It must NOT gain:

- tenant API authority;
- an independent cloud database;
- tenant authorization;
- scheduling authority;
- public O3K resource-ID allocation;
- an independent desired-state model.

Do not build a mini-Neutron daemon.

4. P9 is L3-first but the O3K domain is not permanently L3-only.

P9 may require non-overlapping IPv4 prefixes inside its default routed
`AddressRealm`. That is a P9 profile limitation, NOT a global invariant.
Preserve explicit capability/model seams for future overlapping realms, VRFs,
regional L2 adjacency, overlays and richer providers.

5. First dataplane: boring Linux networking.

Use the smallest stable Linux realization that satisfies accepted SPEC-0026:

- TAP or equivalent endpoint attachment;
- Linux routes/neighbor/forwarding;
- nftables + conntrack for stateful policy/NAT;
- SNAT/DNAT;
- proxy ARP/NDP or equivalent public-address neighbor handling where required;
- `tc` only when the selected behavior requires it.

Prefer structured kernel/Netlink interfaces and transactional/batched updates
where practical. Do not build a custom eBPF dataplane in P9. Do not require
OVS/OVN.

6. No mandatory central network node for normal P9 traffic.

Normal north/south connectivity should be realized on the endpoint's host when
the selected external-network provider supports it. Do not introduce a
Neutron-like shared L3 gateway agent merely because Neutron historically has
one. Explicit centralized-gateway profiles can exist later if a real product
requirement needs them.

7. Compile desired state into semantic `NodeNetworkPlan` intent.

Do NOT create an application provider interface that merely mirrors Neutron:

- `create_router()`
- `create_security_group()`
- `create_floating_ip()`

Instead compile canonical desired state into a versioned semantic plan with
intents such as:

- EndpointAttachment
- AddressAssignment
- RouteIntent
- NeighborIntent
- NatIntent
- PolicyIntent
- AdvertisementIntent
- QoSIntent only when selected later
- EncapsulationIntent only for a future provider/profile

Plans carry stable plan/resource/operation/generation/agent/fencing identity,
deadline and canonical payload fingerprint. Raw provider instructions are
forbidden from canonical application state.

8. Future providers must remain possible without a domain rewrite.

The same canonical state should later be compilable to:

- eBPF TC/XDP/maps/programs;
- OVN/OVS;
- EVPN/VXLAN/Geneve;
- P11 host-level WireGuard routed fabric;
- BGP route/public-address advertisement where the physical network supports it.

Provider portability does not mean zero-disruption live migration of active
stateful flows between nftables/conntrack and a future eBPF connection tracker.
Do not promise or implement that in P9.

9. Neutron is a compatibility projection.

Conceptual mappings may include:

- network -> O3K Network/connectivity domain
- segment -> Segment
- subnet -> Prefix/AddressPool
- port -> Endpoint/Attachment
- router/interface -> route/gateway intent projection
- floating IP -> PublicAddressBinding
- security group/rule -> NetworkPolicy/PolicyRule
- address group -> selector/address set
- QoS -> future QoSPolicy profile
- trunk -> future multiplexed attachment profile

Freeze exact OpenStack method/path/fields/errors/policy/version semantics in the
compatibility manifest and fixtures BEFORE adding handlers. Use official
OpenStack documentation/public client behavior as compatibility authority.
Never mark an operation supported merely because an internal object exists.

P9 SLICES

Execute in this order unless the current repository already contains a proven
slice. Inspect before creating issues.

SLICE 1 — Canonical network intent and durable control-plane state

Goal:
- freeze the exact P9 v1 domain values, resource lifecycle and provider
  capabilities;
- add/extend durable repository ports and migrations for only the P9 state
  actually required;
- define ActionId/ResourceType/ownership/quota/audit vocabulary;
- implement planner/compiler tests producing technology-independent
  NodeNetworkPlan;
- add planned compatibility records/fixtures for the selected P9 Neutron
  operations without advertising them as supported.

Hard rule:
No nftables/eBPF/OVN types in canonical domain/store/application contracts.
No host mutation in this slice unless the issue explicitly owns a small
contract-conformance prerequisite.

Tests:
- resource ownership and cross-project denial;
- address allocation/idempotency/concurrency;
- non-overlap restriction inside P9 routed realm;
- architecture permits a separate future realm capability for overlap;
- deterministic NodeNetworkPlan/fingerprint;
- unsupported provider capability rejected before mutation;
- restart reconstruction from SQLite and PostgreSQL through the existing store
  conformance model;
- quota reservation/release where selected;
- compatibility records remain `planned`/unsupported.

SLICE 2 — Activate the node-local `o3k-network` execution boundary

Goal:
- create/activate the dedicated network execution binary/process using the
  accepted execution protocol, mTLS, agent identity/epoch, command/plan
  acceptance, durable replay, controller work leases and fencing;
- move the existing flat TAP/bridge/DHCP realization behind this boundary with
  no public behavior drift;
- independently document/minimize network privileges;
- keep libvirt/QEMU execution in `o3k-compute`.

Do not duplicate the mature P7 coordination model. Reuse it.

Tests/evidence:
- protocol/capability conformance;
- stale controller token rejected;
- stale network-agent epoch rejected;
- duplicate equivalent plan replayed without mutation;
- same identity/different fingerprint rejected;
- abrupt agent restart/reconnect/resync;
- current flat real-guest networking still passes;
- existing foreign bridge/TAP/DHCP protections remain passing.

SLICE 3 — Routed external-network egress / SNAT

Goal:
- explicit operator-owned external network/uplink/address-pool configuration;
- node-local forwarding/routing and controlled SNAT for an authorized tenant
  network;
- public control-plane state remains provider-independent.

Tests/evidence:
- tenant cannot promote arbitrary network to external;
- external/uplink validation fails closed;
- VM with egress enabled reaches deterministic external target;
- egress-disabled VM does not;
- restart/reconcile restores exact state;
- no broad host route/firewall flush;
- foreign route/address/firewall fixtures remain unchanged;
- partial realization and unknown outcome reconcile safely.

SLICE 4 — Public/floating address lifecycle

Goal:
- durable public-address allocation and project ownership;
- associate/disassociate/reassociate to an authorized endpoint;
- node-local inbound/outbound realization using the P9 provider;
- exact bounded Neutron floating-IP compatibility subset.

Tests/evidence:
- allocation concurrency/no duplicate public address;
- cross-project binding/existence isolation;
- association replay/idempotency;
- interruption at every mutating phase;
- public traffic reaches the intended real VM;
- disassociation removes reachability;
- delete releases address exactly once;
- restart/controller takeover does not duplicate DNAT/neighbor state;
- foreign address/NAT state unchanged.

SLICE 5 — Stateful network policy / security-group projection

Goal:
- canonical NetworkPolicy/PolicyRule with typed selectors;
- exact bounded ingress/egress protocol/port/CIDR or endpoint/group semantics
  frozen by the issue;
- Linux nftables realization;
- exact bounded Neutron security-group compatibility projection.

Tests/evidence:
- authorize before policy mutation;
- cross-project rule/reference denial;
- default behavior explicitly tested;
- allowed flow succeeds from a real traffic source;
- denied flow fails;
- established/related return traffic matches the frozen stateful contract;
- policy update does not silently fail open;
- rule replay/restart/reconcile deterministic;
- no mutation of foreign firewall tables/chains/sets/rules.

SLICE 6 — P9 failure/evidence/claim closure

Run the complete SPEC-0026 failure matrix, including:

- graceful and abrupt o3kd loss;
- controller takeover where the deployment profile supports it;
- graceful and abrupt o3k-network loss;
- transport loss after accepted mutation;
- duplicate/conflicting replay;
- stale fencing token/agent epoch;
- partial NAT/policy realization;
- interrupted public-address association/delete;
- external/uplink outage and recovery;
- foreign same-name/similar resources;
- policy update under real traffic;
- complete cleanup and independent foreign-state comparison.

Run the public real-guest workflow:

authenticate
-> create private network/subnet/endpoint
-> boot VM
-> prove fixed IP
-> enable routed external egress
-> prove external connectivity
-> allocate/associate public address
-> prove inbound connectivity
-> allow one policy flow and prove success
-> remove/deny it and prove failure
-> restart/take over supported components
-> prove same identities and reconverged behavior
-> disassociate/delete public address
-> delete VM/network state
-> prove owned leaks=0, inconsistencies=0, foreign changes=0.

Only after the required evidence passes:
- promote the exact Neutron operations from unsupported/planned to the verified
  compatibility state;
- update product-profile/release claims narrowly;
- do not infer support for VLAN/VXLAN/OVN/eBPF/IPv6 breadth/trunks/SR-IOV/P11.

EXPLICIT P9 NON-GOALS

Do NOT implement in P9 unless a later human-accepted scope change says so:

- broad Neutron parity;
- custom eBPF dataplane;
- OVS/OVN dependency;
- VXLAN/Geneve/EVPN overlays;
- runtime support for overlapping CIDRs;
- tenant VLAN networks;
- trunks/QinQ/VLAN transparency;
- SR-IOV/hardware offload;
- broad QoS;
- full IPv6 parity;
- P11 cross-host WireGuard/BGP/overlay fabric;
- seamless live dataplane-provider migration;
- LBaaS/VPNaaS/DNS/service chaining/IDS/IPS/DDoS products;
- P10 storage or P12 native API work;
- unrelated Cloud Kernel/infrastructure abstraction work.

FAILURE/SECURITY RULES

For every network mutation:

Principal x Action x Resource x Context -> Allow | Deny

Authorization must happen before provider mutation.

Persist intent/phase before side effects where recovery requires it.
A timeout after mutation is UNKNOWN OUTCOME.
Observe before retrying a mutation that could duplicate or destroy state.
Use deterministic operation/plan/command identity.
Use existing controller work leases/fencing and agent epochs.
Reject stale generations/observations.
Never authorize from provider-native names or paths.
Never adopt/delete foreign network resources by name alone.
Never log tokens, keys, unrestricted network config, secret-bearing provider
payloads, or unbounded packet data.
Cleanup must be ownership-checked and reverse-dependency-aware.

TEST/VALIDATION DISCIPLINE

Before completing every child PR, run the repository-required commands:

python3 scripts/check-architecture-boundaries.py
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features

Also run the closest network/store/API/protocol/TestLab suites required by the
child issue.

For privileged/real-host claims, run only the evidence tier required by the
accepted issue/spec and bind the evidence to the exact tested source. Do not
promote fake, skipped, portable, repository-only or stale artifacts to real-host
evidence.

CLEAN IMPLEMENTATION

Use only public O3K repository state, official OpenStack/public Linux/kernel
specifications and independently produced black-box behavior. Public Go O3K is
non-normative and may be inspected only under ADR-0151. Do not use private
employer/customer/OpenStack-derived implementation material.

WHEN YOU START

Do not begin by editing code.

First return a concise current-state report containing:

1. exact main commit inspected;
2. ADR-0168/SPEC-0026 acceptance status;
3. #655 state and existing child issues/PRs;
4. current Network/Neutron compatibility state;
5. existing o3k-network/network-provider/store/protocol implementation that can
   be reused;
6. which P9 slice is the first genuinely incomplete dependency;
7. proposed single coherent child issue with acceptance criteria;
8. explicit non-goals for that child issue.

If the architecture gate is Accepted and no existing issue already owns that
slice, create the coherent child issue and then implement only that slice.
If an issue already owns it, use that issue rather than creating a duplicate.
```
