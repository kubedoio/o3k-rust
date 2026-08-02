# ADR-0154 — Engineering governance lifecycle

Status: Proposed
Date: 2026-08-02
Review date: 2026-09-01
Responsible maintainer: O3K maintainers
Supersedes: none
Superseded-by: none

This is a proposal. It is not an accepted architecture, public-contract, or
release-governance decision until a human maintainer accepts it in a reviewed
pull request. The rules below are requirements for the governance work, not
evidence that the corresponding automation or artifacts already exist.

## Context

Issue [#332](https://github.com/kubedoio/o3k-rust/issues/332) identified four
related control gaps: the API contract did not have a governed lifecycle,
decisions did not have a machine-checkable supersession policy, the exact
toolchain needed a single reproducibility rule, and test output did not have a
uniform evidence vocabulary. The static target in
[ADR-0153](ADR-0153-static-rust-and-openstack-release-policy.md) addresses the
Rust and OpenStack target values, but does not establish those lifecycle
controls.

## Decision proposal

Adopt three linked, single-purpose policy surfaces:

1. [SPEC-0017](../specs/SPEC-0017-openapi-contract-governance.md) governs
   OpenAPI descriptions and compatibility-facing contract changes.
2. [SPEC-0018](../specs/SPEC-0018-toolchain-and-test-evidence-governance.md)
   governs reproducible toolchains, test tiers, and evidence states.
3. This ADR governs decision records and the human authority boundary.

The policies are deliberately additive. They do not change the OpenStack
target manifest, API baseline, compatibility tests, or implementation behavior.
Those artifacts remain governed by their existing accepted specifications.

### ADR lifecycle

Each ADR has one decision and records `Status`, date, owner, context, options
considered, decision, consequences, and sources. Status values are exactly:

`Draft`, `Proposed`, `Accepted`, `Rejected`, or `Superseded`.

An `Accepted` ADR is immutable in substance. A changed decision requires a new
ADR whose `Supersedes` field names the prior ADR; metadata corrections may be
made without changing the decision. A superseded ADR remains readable and
must link to its successor. An ADR must not claim acceptance based only on an
agent-authored commit, passing CI, or an issue state.

Architecture, security, licensing, privileged/native execution, persistence,
public-contract, and release-governance ADRs require an identified human
maintainer approval before their status can become `Accepted`.

### Fitness functions and traceability

An accepted ADR links at least one executable check when a check is practical.
If no executable check is practical, it records `not-applicable` and explains
why. A policy or implementation PR must identify the affected requirement,
contract, tests, and evidence state; links must not be invented merely to fill
out a template.

## Acceptance checks

This proposal is ready for acceptance only when a reviewer can verify all of
the following against the repository, without relying on an agent's claim:

- every ADR has a parseable status and exactly one decision heading;
- status values are limited to the five values above;
- every `Supersedes`/`Superseded-by` link resolves and the graph has no cycle;
- two active ADRs do not make conflicting decisions about the same named
  subject, or one explicitly identifies the conflict for human resolution;
- accepted high-risk ADRs identify a human approval in the PR/review record;
- each accepted ADR has an executable fitness function or a justified
  `not-applicable` record;
- policy links in this ADR and [the ADR index](README.md) resolve;
- an ADR check fails closed on malformed headers, duplicate identifiers,
  dangling supersession links, or an unapproved high-risk acceptance.

Until those checks are implemented and the proposal is accepted, this file
must be reported as `Proposed`, not as an active completed governance gate.

## Consequences

Reviewers can distinguish a design proposal, an accepted decision, and an
observed result. Agents may prepare ADRs and evidence, but cannot promote
either to human approval. The cost is a small amount of metadata and review
work for every significant decision; the benefit is that a later change does
not silently redefine a public contract or release claim.

## Sources

- [Repository agent contract](../../AGENTS.md)
- [Public Go reference policy](ADR-0151-public-go-o3k-reference-policy.md)
- [Static Rust/OpenStack target](ADR-0153-static-rust-and-openstack-release-policy.md)
- [Issue #332](https://github.com/kubedoio/o3k-rust/issues/332)
