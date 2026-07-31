# ADR-0010: Isolated DHCP ownership boundary

## Status

Accepted

## Decision

O3K writes one durable state file and one dnsmasq configuration under an
O3K-owned directory. Bindings are keyed by Neutron port ID and validated for
unique MAC and IPv4 address before the state is atomically replaced. The
dnsmasq supervisor receives only that generated configuration and pid-file;
it does not inspect or modify system-wide dnsmasq configuration.

## Rationale

This gives restart recovery and deterministic fixed leases while protecting
operator-managed DHCP services. The command boundary is intentionally small;
privilege, process isolation, and real libvirt reachability remain deployment
and TestLab concerns rather than being hidden in the metadata store.

## Consequences

The service rejects invalid or conflicting subnet settings before writing a
reloadable configuration. Deleting a binding removes it on the next generated
configuration. A real dnsmasq/libvirt integration test is required on hosts
that provide those services.
