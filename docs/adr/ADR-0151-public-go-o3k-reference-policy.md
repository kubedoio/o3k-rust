# ADR-0151 — Public Go O3K as a non-normative reference

Status: Accepted

## Context

O3K Rust needs a complete compatibility plan before protected infrastructure
testing. The public Apache-2.0 `kubedoio/o3k` repository contains useful route,
client, test, and operational experience, but it has different architecture and
is not an OpenStack specification. Treating it as either forbidden or
authoritative would make requirements discovery slower or unsafe.

## Decision

Use sources in this order:

1. official OpenStack API documentation and published specifications;
2. public OpenStack client, SDK, Terraform, and Tempest behavior;
3. O3K Rust ADRs, contracts, tests, and protected black-box evidence;
4. the public Apache-2.0 Go O3K repository as a non-normative secondary reference.

The Go repository may be inspected for route and extension inventory,
requirements and request/response field discovery, failure and cleanup
scenarios, operational lessons, and black-box behavioral comparison. A Go
behavior that conflicts with an official OpenStack contract is not adopted
without an explicit Rust decision grounded in the official contract.

Mechanical Go-to-Rust translation, copying the Go architecture, and use of
private or unpublished material remain prohibited. Code, tests, or fixtures may
only be reused when their Apache-2.0 provenance is verified and recorded with
the exact source commit, paths, copyright/NOTICE attribution, and changes made.
When no artifact is reused, the PR records that fact explicitly.

Each issue or PR that consults Go records:

- repository URL and pinned commit;
- inspected routes, handlers, tests, fixtures, or operational files;
- official OpenStack sources used to decide expected behavior;
- copied/adapted artifacts and their attribution, or `none`;
- unresolved differences and the decision owner.

Requirements found through Go are rewritten as independent Rust contracts and
black-box tests. The Rust domain, provider, compute-agent, reconciliation, and
ownership architecture remains independent.

## Consequences

Compatibility planning can use public operational experience without making
the protected runner the requirements-discovery loop. Provenance review is a
merge requirement, and generated or adapted material must remain auditable.

## Public references

- [OpenStack API documentation](https://docs.openstack.org/api-quick-start/)
- [OpenStack API references](https://docs.openstack.org/2026.1/api/)
- [Nova Compute API reference](https://docs.openstack.org/api-ref/compute/)
- [Public Go O3K repository](https://github.com/kubedoio/o3k)
- [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0)
