# ADR-0129 — Make the portable real-libvirt harness model cleanup state

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, image, cli, governance

## Context

The portable `real-libvirt-harness.sh` test used a fake OpenStack CLI that
returned success for dependent-resource operations without tracking resource
state. The cleanup contract correctly requires verified absence, so the
harness could fail for a fixture defect and was not part of normal CI.

## Decision

The fake CLI tracks image, keypair, network, subnet, flavor, and server state.
Create commands publish deterministic IDs, show commands report absence after
delete, and the CI workflow runs the harness contract test. The fixture does
not weaken cleanup assertions or represent real-host evidence.

## Consequences

Portable CI catches regressions in the cleanup contract while keeping the
real-host lifecycle and CirrOS acceptance explicitly host-gated.
