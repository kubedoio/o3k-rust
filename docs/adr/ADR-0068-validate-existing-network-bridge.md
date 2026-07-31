# ADR-0068 — Validate existing bridge and uplink ownership

## Context

The host network manager returned success whenever the configured bridge name
already existed. A down bridge or an uplink absent from that bridge could then
be reused for a VM TAP interface without validation.

## Decision

When a bridge already exists, bring it up and require every configured uplink
to be present and attached to that bridge. A missing uplink is a command
failure; an existing uplink attached elsewhere is treated as foreign and
blocks TAP creation.

## Consequences

Existing host networking is no longer silently adopted in an unusable or
foreign state. The manager may restore the managed bridge's `UP` state, but it
does not hijack an uplink attached to another bridge.
