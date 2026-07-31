# ADR-0083 — Restrict systemd cleanup to the default system layout

## Status

Accepted

## Context

`packaging/install.sh` installs systemd units and enables services only for the
default layout: `/usr/local`, `/var/lib/o3k`, `/etc/o3k`, and `/var/log/o3k`.
The uninstall script previously stopped or disabled the O3K service names,
reloaded systemd, and removed the units under `/etc/systemd/system` for every
prefix. That allowed a custom-prefix or release-bundle uninstall to affect
system state unrelated to that installation.

## Decision

`packaging/uninstall.sh` computes the same exact four-path default-layout
predicate as the installer. It performs `systemctl disable --now`,
`systemctl daemon-reload`, and removal of the `/etc/systemd/system` O3K units
only when that predicate matches. Binary and helper cleanup remains scoped to
the requested prefix, and purge ownership checks remain unchanged.

## Consequences

Normal default-layout uninstall retains its existing service and unit cleanup.
Custom-prefix and release-bundle uninstall cannot target O3K-named systemd
services or unit files. The packaging safety test uses a deterministic
`systemctl` log and a host-unit removal sentinel to protect this boundary.
