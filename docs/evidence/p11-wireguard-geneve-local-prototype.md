# P11 WireGuard/Geneve local prototype evidence

Status: prototype evidence only; not a P11 support or release gate

Date: 2026-08-20

## Scope

The executable scenario
`scripts/p11-wireguard-geneve-overlap.sh` creates three isolated Linux host
namespaces, one shared WireGuard transport per host, and realm-scoped
known-unicast Geneve routes. Realm A and Realm B both use `10.0.0.0/24` and
the endpoint addresses `10.0.0.10` and `10.0.0.20`.

It verifies:

- WireGuard host transport convergence;
- Realm A and Realm B traffic with VNI 101 and VNI 102;
- zero observed cross-realm endpoint misdelivery;
- no tenant address observed in the underlay UDP capture;
- host-boundary policy deny and recovery;
- a 1400-byte tenant-MTU near-boundary ping with DF set;
- no delivery path for an unattached VNI;
- exact namespace/link cleanup.

Command:

```text
scripts/p11-wireguard-geneve-overlap.sh
```

Observed output on the local Linux host:

```text
p11-wireguard-geneve-overlap: wireguard-host-transport=passed
p11-wireguard-geneve-overlap: realm-a-overlap-traffic=passed
p11-wireguard-geneve-overlap: realm-b-overlap-traffic=passed
p11-wireguard-geneve-overlap: cross-realm-misdelivery=0
p11-wireguard-geneve-overlap: cleartext-underlay-tenant-packets=0
p11-wireguard-geneve-overlap: policy-deny-and-recovery=passed
p11-wireguard-geneve-overlap: mtu-boundary=passed
p11-wireguard-geneve-overlap: wrong-vni-delivery=0
p11-wireguard-geneve-overlap: cleanup=passed
```

The local `o3kd` process was also started with an isolated temporary data
directory on `127.0.0.1:18080`; `/healthz` returned `{"status":"ok"}` and
`/readyz` returned `{"status":"ready"}`.

## Explicit limits

Linux namespaces are not three independent KVM/libvirt hypervisors. This
evidence does not establish the mandatory full P11 gate, nor does it prove
canonical P9 FIP/NAT integration, durable P10 LVM/Ceph behavior, controller
restart/fencing, host drain, peer interruption recovery, or foreign-state
mutation counters. The policy rule in this prototype is a kernel execution
primitive, not a substitute for the canonical control-plane NetworkPolicy
workflow. No private WireGuard key bytes are included in this document or the
scenario output.
