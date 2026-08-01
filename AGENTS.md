# AGENTS.md — mandatory instructions for coding agents

This file is the top-level operating contract for every LLM or automated coding agent working in O3K Rust.

## Mission

Build a small, understandable, secure, and testable OpenStack-compatible control plane in Rust. The first product is O3K TestLab. Correct end-to-end behavior matters more than endpoint count.

## Authority order

When instructions conflict, use this order:

1. security, licensing, privacy, and clean-implementation rules;
2. accepted ADRs in `docs/adr/`;
3. normative specs in `docs/specs/`;
4. public contracts under `contracts/` and `proto/`;
5. official OpenStack documentation and published specifications;
6. public OpenStack client, SDK, Terraform, and Tempest behavior;
7. O3K Rust contracts, tests, and black-box evidence;
8. public Go O3K as a non-normative secondary reference;
9. issue acceptance criteria;
10. tests;
11. existing implementation.

Do not silently resolve a conflict. Stop, describe it in the issue or PR, and propose the smallest corrective change.

For OpenStack compatibility questions specifically, use this source priority:

1. official OpenStack API documentation and published specifications;
2. public OpenStack client, SDK, Terraform, and Tempest behavior;
3. O3K Rust ADRs, contracts, tests, and black-box evidence;
4. public Go O3K as a non-normative secondary reference.

## Mandatory reading order

Before editing code, read:

1. `README.md`;
2. `docs/PROJECT_CHARTER.md`;
3. `docs/CLEAN_IMPLEMENTATION.md`;
4. `docs/ARCHITECTURE.md`;
5. `docs/TEST_STRATEGY.md`;
6. the relevant ADR and SPEC;
7. the assigned issue.

## Issue-driven rule

- One issue is the unit of intent.
- One PR should normally close one issue.
- Do not implement an untracked feature.
- Do not expand scope because a nearby improvement looks easy.
- Missing requirements must become an issue or spec amendment.

## Required agent plan

Before code changes, post or record:

- the issue being solved;
- files expected to change;
- contracts and specs affected;
- public reference inputs, pinned revisions, and provenance records;
- tests to add first;
- known uncertainties;
- explicit non-goals.

## Implementation rules

- Prefer explicit domain types over strings and maps.
- Model lifecycle transitions as validated state machines.
- Keep HTTP/OpenStack representations outside the core domain.
- Keep provider-specific types outside the core domain.
- Use idempotency keys or deterministic operation identities for retriable mutations.
- Persist intent before executing external side effects where the spec requires recovery.
- Treat timeouts as unknown outcomes, not automatic failures.
- Avoid `unsafe`; each use requires a dedicated ADR, safety comments, tests, and reviewer approval.
- Avoid global mutable state.
- Never log secrets, tokens, image credentials, private keys, or unredacted provider payloads.
- Every background task must have shutdown, retry, backoff, and observability behavior.
- New dependencies require justification, license compatibility, maintenance review, and minimal feature selection.

## Clean implementation restrictions

Agents must not:

- copy, translate, summarize into code, or structurally reproduce non-public source code;
- use internal SAP, CobaltCore, NeoNephos, customer, or employer documents as implementation inputs;
- claim behavior based on memory when a public specification or executable test is available;
- use another implementation's private tests, schemas, migration files, or internal architecture;
- add generated code whose source contract is not committed;
- remove attribution or license notices.

Public OpenStack documentation, public API schemas, public SDK behavior, public standards, and independently written black-box tests are allowed inputs. The public Apache-2.0 Go O3K repository may also be used as a non-normative secondary reference under [ADR-0151](docs/adr/ADR-0151-public-go-o3k-reference-policy.md). Record every inspected Go path, pinned commit, and official source in the issue/PR or the affected spec.

## Test-first acceptance

A change is incomplete until it has appropriate tests:

- domain invariant tests;
- protocol/contract tests;
- provider fake tests;
- failure-path tests;
- regression tests for bugs;
- integration tests for public behavior.

Mocks that only repeat implementation expectations are not evidence. Prefer stateful fakes, real databases, process-level tests, and black-box HTTP tests.

## Commands required before completion

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Run additional contract or integration commands named by the issue. Report commands and results in the PR.

## PR response format

Every agent-authored PR description must include:

- **Issue:** linked issue;
- **Intent:** one paragraph;
- **Design:** important choices and rejected alternatives;
- **Contracts:** changed or unchanged;
- **Tests:** exact commands and important cases;
- **Risks:** unresolved concerns;
- **Provenance:** public sources and AI tools used;
- **Follow-ups:** separate issues, not hidden TODOs.

When the public Go O3K repository is consulted, the PR must additionally record:

- the repository URL and exact commit;
- Go files, handlers, routes, tests, or fixtures inspected;
- whether any artifact was copied or adapted (normally none);
- Apache-2.0 attribution and NOTICE handling for every reused artifact.

Go behavior is requirements and comparison evidence only. Official OpenStack
specifications and public client behavior remain normative, and mechanical
translation or reproduction of Go architecture is prohibited unless a separate
accepted decision explicitly authorizes it.

## Definition of done

- acceptance criteria satisfied;
- tests fail before/fix after when practical;
- formatting, lint, unit, and relevant integration tests pass;
- contracts and documentation updated;
- no unrelated change;
- no unexplained TODO, panic, `unwrap`, or ignored error in production paths;
- compatibility evidence updated when external behavior changes;
- reviewer can understand the change without reconstructing agent reasoning.
