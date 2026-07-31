# ADR-0033 — Record CLI list coverage and resource identities

## Status

Accepted

## Context

The OpenStack CLI acceptance workflow exercised most server lifecycle actions
but omitted `server list`. Its result artifact also did not contain the
resource IDs created during the run, making cleanup review and failure
diagnosis dependent on unredacted logs. The public CLI does not provide the
operation IDs assumed by the earlier comment.

## Decision

Run `openstack server list` as part of the lifecycle and retain its response as
a separate local diagnostic file. Include only created resource IDs in the
redacted result artifact, including on failure, and state explicitly that
operation IDs are not claimed when the public CLI does not expose them.

## Consequences

The release artifact proves that list was attempted and reviewers can correlate
cleanup with the created image, network, subnet, flavor, and server without
receiving response bodies or credentials. Full boot, fixed-IP, operation
tracking, and real-libvirt evidence remain separate requirements.
