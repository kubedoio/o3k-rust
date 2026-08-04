# ADR-0030 — Own the dnsmasq process lifecycle

Status: Accepted for the portable DHCP process-lifecycle slice.
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: network, governance

## Context

`DhcpService::start` previously used `Command::status`, which blocked until
dnsmasq exited and gave callers no way to determine liveness, restart after a
configuration publication, or stop the process safely. A pid file alone is
not sufficient ownership evidence because it can be stale or belong to a
different process.

## Decision

Starting DHCP now publishes the validated configuration and returns a
`DnsmasqSupervisor` that owns the spawned child handle. The supervisor offers
liveness checks, restart, and stop operations, and removes only its managed
pid-file path. Dropping the supervisor stops the child on a best-effort basis.
Reload is implemented as a controlled restart so this portable slice does not
depend on signal or privilege-specific behavior.

## Consequences

The caller can supervise and retry the managed dnsmasq process without
blocking on its lifetime, and configuration changes have an explicit restart
boundary. This does not yet integrate TAP/libvirt networking, dnsmasq leases,
system service supervision, or privileged-host/guest DHCP evidence.
