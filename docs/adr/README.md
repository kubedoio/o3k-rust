# Architecture decision records

This directory is the source location for O3K Rust ADRs. The lifecycle rules
below are proposed by [ADR-0154](ADR-0154-engineering-governance-lifecycle.md)
and are not evidence that the repository has already passed the corresponding
automated audit.

## Status vocabulary

Every ADR must declare exactly one of:

`Draft`, `Proposed`, `Accepted`, `Rejected`, or `Superseded`.

Accepted decisions are immutable in substance. A changed decision gets a new
ADR with a `Supersedes` link; a superseded record remains available and links
to its successor. Architecture, security, licensing, privileged/native,
persistence, public-contract, and release-governance decisions require human
approval before `Accepted`.

## Governance entrypoints

| ADR | Subject | Status |
| --- | --- | --- |
| [ADR-0151](ADR-0151-public-go-o3k-reference-policy.md) | Public Go O3K reference boundary | Accepted |
| [ADR-0153](ADR-0153-static-rust-and-openstack-release-policy.md) | Static Rust/OpenStack target | Accepted |
| [ADR-0154](ADR-0154-engineering-governance-lifecycle.md) | Decision and evidence governance | Proposed |
| [ADR-0155](ADR-0155-agent-local-image-materialization.md) | Agent-local verified image materialization | Proposed |

The table is an entrypoint for the governance decisions; it is not a claim
that every historical ADR has already been machine-audited. Until the audit is
implemented, reviewers must inspect the affected ADR files directly.

## Required audit

The ADR audit is accepted only when it can demonstrate, from repository state:

- unique identifiers and parseable required metadata;
- status values limited to the vocabulary above;
- resolvable supersession links and an acyclic graph;
- no duplicate active decision for the same subject without an explicit
  conflict record;
- a fitness function or justified `not-applicable` entry for every accepted
  decision where practical;
- recorded human approval for accepted high-risk decisions.

Malformed metadata, dangling links, cycles, duplicate active decisions, and
unapproved high-risk acceptance must fail closed. A passing link check alone is
not ADR acceptance.
