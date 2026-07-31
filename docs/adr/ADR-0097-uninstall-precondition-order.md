# ADR-0097 — Validate purge ownership before service mutation

## Status

Accepted

## Context

`packaging/uninstall.sh --purge` must refuse an unowned data, configuration,
or log directory. Previously the default-layout branch stopped and disabled
the O3K systemd units before performing that ownership check. A rejected purge
could therefore change service state even though no state was eligible for
removal. This is especially difficult to diagnose on a clean Debian host,
where a failed destructive command should leave the service running.

## Decision

Perform all purge path and ownership preconditions before any systemd stop,
disable, or daemon-reload operation. Only after those checks pass may the
default-layout uninstall perform service cleanup and remove O3K-owned state.
The non-purge uninstall path retains its documented service cleanup and state
preservation behavior.

## Consequences

A rejected purge is side-effect free with respect to O3K services. The
ownership policy and exact default-layout systemd boundary remain unchanged.
The packaging regression test uses a fake `systemctl` and an unowned target to
prove that no service command is issued before rejection.

## Provenance

This is an independently authored repository decision based on issue #90,
the packaging ownership-marker contract, and ADR-0083. No private source or
implementation was used.
