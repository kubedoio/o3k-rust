# SPEC-0006 — Flat network, subnet, and port subset

Status: Implemented subset

O3K exposes the Neutron-shaped `/v2.0` network, subnet, and port routes for
the TestLab profile. Resources are project-scoped by the verified token.

The initial provider is flat and local. IPv4 CIDRs from `/0` through `/30`
are accepted; network and broadcast addresses are reserved. A deterministic
gateway and allocation pool are derived when omitted, and ports receive the
lowest available address. Routers, IPv6, security groups, floating IPs,
provider networks, and port updates are out of scope.

Network metadata is persisted atomically under the configured data directory.
This alpha adapter uses a process mutex and durable JSON metadata; a future
multi-process deployment must move allocation into the SQLite store with a
unique subnet/IP constraint before enabling concurrent writers.

Evidence is provided by the network unit tests and
`neutron_network_subnet_port_lifecycle_is_deterministic`.
