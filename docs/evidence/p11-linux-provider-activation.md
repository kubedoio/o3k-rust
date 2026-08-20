# P11 Linux provider activation evidence

Status: bounded provider evidence; not a full P11 support gate

## Scope

The accepted P11 Linux provider is now selectable by the network execution
agent with `O3K_NETWORK_P11_ROOT`. Plans containing `p11_fabric` are routed to
the fenced `LinuxP11FabricBackend`; they are not silently sent to the shared
flat bridge. The provider realizes local endpoint TAPs on the realm-scoped
bridge, remote realm proxy-neighbor entries, Geneve attachments, and the
shared WireGuard host transport.

The fabric plan now carries the accepted realm prefix into the provider. The
provider derives the realm-local gateway on the realm veth, so overlapping
realms can use the same tenant prefix without a shared host route table. Public
address bindings are carried in the same canonical snapshot. For each binding,
the provider uses a realm-specific transit veth, policy-routed public /32,
proxy-ARP on the configured uplink, and realm-namespace DNAT/SNAT. Public NAT
is therefore scoped to the owning realm and endpoint rather than implemented as
a bare host-wide address rule.

When a P11 plan carries the same canonical policy snapshot as its policy
intents, the agent admits that snapshot to the Linux provider. The provider
persists the policy generation and SHA-256 fingerprint before replacing an
owned realm-network nftables table, rejects foreign tables, and removes the
owned table during realm cleanup.

Provider ownership records include endpoint TAP state and a durable pending
TAP set written before link mutation. Existing links without a matching
durable ownership record are rejected as foreign. Private WireGuard key bytes
remain host-local and are excluded from plans and observations.

## Evidence

Commands run from the repository root:

```text
cargo fmt --all -- --check
cargo clippy -p o3k-network --all-targets --all-features -- -D warnings
cargo test -p o3k-network --all-features linux_p11::tests -- --nocapture
O3K_P11_SMOKE_ROOT=/tmp/o3k-p11-smoke-XXXX target/debug/examples/p11-linux-smoke
O3K_P11_FIP_ROOT=/tmp/o3k-p11-fip-smoke-XXXX \
  O3K_P11_FIP_UPLINK=o3k-fip-u \
  cargo run -p o3k-network --example p11-linux-fip-smoke --all-features
```

Observed provider smoke results:

```text
p11-linux-smoke: host-transport-address=passed
p11-linux-smoke: geneve-realization=passed
p11-linux-smoke: isolated-attachment=passed
p11-linux-smoke: topology-and-cleanup=passed
p11-linux-fip-smoke: realm-scoped-public-traffic=passed
p11-linux-fip-smoke: cleanup=passed
```

The three-host namespace overlap harness also continues to pass its existing
WireGuard, Geneve, overlap-isolation, policy, MTU, wrong-VNI, and cleanup
checks. That harness remains portable namespace evidence, not the mandatory
three-independent-KVM/libvirt gate.

## Explicit limits

This remains bounded provider evidence. The disposable smoke proves one local
realm-scoped public binding with real namespaces, bridge delivery, DNAT/SNAT,
and cleanup; it is not evidence for real external FIP traffic across
independent KVM/libvirt hosts. It does not prove local bridge anti-spoof/policy
enforcement, storage placement/checksum persistence, restart/failure recovery,
host drain, or the final zero-leak evidence matrix. The public uplink and the
client-side neighbor are explicitly configured by the smoke, so this is not a
production external-network or proxy-ARP acceptance gate.
