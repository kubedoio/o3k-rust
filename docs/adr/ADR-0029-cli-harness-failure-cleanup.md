# ADR-0029: Clean up resources after CLI harness failures

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, image, cli, governance

## Context

The OpenStack CLI/libvirt acceptance harness creates an image, network,
subnet, flavor, and server. With shell fail-fast enabled, a failed lifecycle
command could terminate the script before the remaining resources were
deleted, and no failure artifact was written.

## Decision

Track each created resource and install an exit handler. On an unsuccessful
run, the handler deletes known resources in dependency order, records whether
cleanup passed or failed, and writes a redacted `failed` artifact. Successful
runs clear each identifier immediately after deletion. Skipped prerequisite
runs remain explicit `skipped` artifacts and do not attempt cleanup.

## Consequences

Interrupted or failed acceptance runs are diagnosable and converge toward
clean test infrastructure. A cleanup failure remains visible and cannot be
mistaken for release evidence because the release gate requires `status:
passed` and `cleanup.status: passed`.
