# P11 Linux provider activation evidence

Status: bounded provider evidence; not a full P11 support gate

## Scope

The accepted P11 Linux provider is now selectable by the network execution
agent with `O3K_NETWORK_P11_ROOT`. Plans containing `p11_fabric` are routed to
the fenced `LinuxP11FabricBackend`; they are not silently sent to the shared
flat bridge. The provider realizes local endpoint TAPs on the realm-scoped
bridge, remote realm proxy-neighbor entries, Geneve attachments, and the
shared WireGuard host transport.

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
```

Observed provider smoke results:

```text
p11-linux-smoke: host-transport-address=passed
p11-linux-smoke: geneve-realization=passed
p11-linux-smoke: isolated-attachment=passed
p11-linux-smoke: topology-and-cleanup=passed
```

The three-host namespace overlap harness also continues to pass its existing
WireGuard, Geneve, overlap-isolation, policy, MTU, wrong-VNI, and cleanup
checks. That harness remains portable namespace evidence, not the mandatory
three-independent-KVM/libvirt gate.

## Explicit limits

This slice does not yet activate canonical NetworkPolicy, routed/FIP/NAT
realization, or public-address intents inside a P11 plan; those intents fail
closed until their realm-scoped provider path is implemented. It does not
prove real external FIP traffic, independent KVM/libvirt hosts, storage
placement/checksum persistence, restart/failure recovery, host drain, or the
final zero-leak evidence matrix.
