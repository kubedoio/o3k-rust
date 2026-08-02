# ADR-0065 — Reject DHCP gateway and fixed-binding conflicts

Status: Accepted

## Context

DHCP state can be reconfigured after fixed bindings have been published. A
gateway change to an address already assigned to a binding would render both a
default-route directive and a host reservation for the same address.

## Decision

Before publishing a new DHCP configuration, reject any existing binding whose
address equals the proposed gateway. The prior configuration remains active
because validation occurs before state mutation.

## Consequences

Gateway changes must first remove or move the conflicting binding. Reloads
cannot publish contradictory dnsmasq configuration, and failed reconfiguration
does not replace the last valid state.
