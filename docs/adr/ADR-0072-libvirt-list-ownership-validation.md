# ADR-0072: Validate ownership before listing managed libvirt domains

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, governance

## Context

The libvirt domain name prefix is only a naming convention. A foreign domain
can use an `o3k-` name, while malformed or duplicate O3K metadata must not be
selected for reconciliation. The adapter already defines domain XML metadata
and discovery rules, but the backend listing path previously returned names
based on the prefix alone.

## Decision

`list_managed_domains` retrieves each candidate domain's XML and returns only
domains with valid, unique O3K ownership metadata. Foreign, malformed, and
duplicate domain records are excluded. The configured name prefix remains a
compatibility scope, but it is not ownership evidence.

## Consequences

Listing fails closed for ambiguous ownership and cannot accidentally expose a
foreign prefix-matching domain to reconciliation. Domains whose XML cannot be
read are skipped from the managed result. A full host-backed discovery and
reconciliation run remains release-gated by the real-libvirt acceptance
evidence.
