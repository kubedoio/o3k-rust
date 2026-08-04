# ADR-0140 — Fence managed TAPs, DHCP leases, and port MACs

Status: accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: network, governance

O3K network discovery and reuse now query detailed link metadata and require
`tun type tap` in addition to the expected MAC and managed bridge. A
same-named foreign Ethernet or dummy link therefore cannot be adopted or
deleted as an O3K TAP.

The DHCP renderer pins dnsmasq leases to `dnsmasq.leases` below the managed
DHCP root, keeping restart state within the owned directory. Network metadata
rejects duplicate MAC identities when opening persisted state and before
publishing a new port.

This ADR covers repository ownership and metadata invariants for issue #81. It
does not claim the coupled agent-to-libvirt network attachment or real-host
guest IP evidence, which remain pending.
