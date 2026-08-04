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

## Normative source ownership

Read `docs/NORMATIVE_SOURCES.md` before changing architecture, identity,
workflows, compatibility manifests, execution protocols, or deployment
profiles.

- Summary documents explain intent and link to normative sources.
- Do not copy a field-level contract, compensation state machine, or
  authorization rule into multiple summaries.
- When a summary conflicts with the named normative source, correct the summary.
- An external-hosted service must never be described as O3K-implemented.
- Runtime and release claims require executable evidence even when the spec is
  accepted.

## Mandatory reading order

Before editing code, read:

1. `README.md`;
2. `docs/PROJECT_CHARTER.md`;
3. `docs/CLEAN_IMPLEMENTATION.md`;
4. `docs/ARCHITECTURE.md`;
5. `docs/NORMATIVE_SOURCES.md`;
6. `docs/TEST_STRATEGY.md`;
7. the relevant ADR, SPEC, compatibility profile, and execution contract;
8. the assigned issue.

For identity, service topology, provider boundaries, cross-service workflows, hosted-service profiles, or runner changes, also read:

- `docs/adr/ADR-0160-service-topology-and-execution-boundaries.md`;
- `docs/adr/ADR-0161-keystone-trust-and-service-identity.md`;
- `docs/adr/ADR-0162-contract-first-staged-runner-validation.md`;
- `docs/specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md`;
- `docs/specs/SPEC-0021-cross-service-workflows-and-compensation.md`;
- `docs/specs/SPEC-0022-service-api-baseline-and-evidence-gates.md`;
- `docs/specs/SPEC-0023-external-cinder-service-under-test.md` when an external service is involved;
- `contracts/execution-boundaries.md`.

## Issue-driven rule

- One issue is the unit of intent.
- One PR should normally close one issue.
- Do not implement an untracked feature.
- Do not expand scope because a nearby improvement looks easy.
- Missing requirements must become an issue or spec amendment.
- An endpoint outside the accepted compatibility profile must not be implemented opportunistically.
- Do not create a micro-issue when the work is an acceptance criterion of an existing coherent issue.

## Required agent plan

Before code changes, post or record:

- the issue being solved;
- files expected to change;
- contracts and specs affected;
- public reference inputs, pinned revisions, and provenance records;
- the declared service/API profile operation being implemented;
- whether the service is `o3k-implemented` or `external-hosted`;
- cross-service dependencies and compensation phases;
- the required evidence tier: portable, process, component real-host, hosted-service, full-cloud, or release;
- tests to add first;
- known uncertainties;
- explicit non-goals.

## Service and process rules

- `o3kd` initially owns OpenStack-compatible service semantics, authorization, desired state, scheduling, operations, compensation, and reconciliation.
- `o3k-compute`, `o3k-network`, and `o3k-storage` are execution boundaries, not independent sources of public OpenStack truth.
- Logical service/provider separation must precede process separation.
- Do not introduce a new daemon without an accepted ADR covering privilege, identity, protocol, failure domain, deployment, restart, and cleanup.
- Keystone-compatible identity is the common trust and discovery root, not the transaction coordinator for servers, ports, volumes, or allocations.
- Never use a display name where a durable project, user, service, endpoint, resource, or provider ID is required.
- Cinder and `o3k-storage` do not block the first ephemeral-root libvirt guest.
- A real external Cinder deployment retains its own database, message bus, service processes, backend, migrations, and upgrades.

## Compatibility-profile rules

- O3K targets operation-level profiles, not complete OpenStack service parity.
- Every public operation requires a compatibility record with method, path, API version or microversion, auth scope, request/response/error contract, state transition, dependencies, provider capability, tests, and known deviations.
- A route without the required profile record is unsupported even when partial implementation exists.
- Catalog entries advertise only implemented and enabled profiles.
- Catalog and manifests distinguish `o3k-implemented` services from `external-hosted` services.
- Separate the upstream reference maximum, O3K advertised range, implemented range, and verified range.
- Do not advertise `volumev3`, advanced Neutron, metadata HTTP, or broad microversion ranges before their declared gates pass.
- Official OpenStack specifications and public client behavior remain normative.

## Evidence-ladder rules

Work proceeds in this order unless an accepted issue explicitly justifies otherwise:

1. ADR/SPEC/contract and compatibility-profile validation;
2. domain, store, migration, and policy tests;
3. stateful provider or external-service conformance tests;
4. portable simulated-cloud integration;
5. process-level client tests;
6. compute/network/storage component or hosted-service real-host gate;
7. full-cloud real-host gate;
8. restart/failure matrix;
9. release gate.

Rules:

- The protected full-cloud runner is a final integration verifier, not the primary requirements-discovery loop.
- Do not run or modify the full runner to compensate for a missing API/spec/portable test.
- Do not delay all real execution until an undefined goal of complete Nova, Neutron, Cinder, or Keystone parity.
- Component real-host gates must preserve inspectable owned state through a bounded protected diagnostic hold before cleanup when diagnosis requires it.
- A fake-provider, skipped, ready, or repository-only result is not real-host evidence.
- Record implementation, portable evidence, component or hosted-service evidence, full-cloud evidence, and remaining acceptance independently.

## Implementation rules

- Prefer explicit domain types over strings and maps.
- Model lifecycle transitions as validated state machines.
- Keep HTTP/OpenStack representations outside the core domain.
- Keep provider-specific and external-service client types outside the core domain.
- Use one normalized typed `AuthContext` for all service authorization.
- Preserve original user/project audit context and authenticated service identity for cross-service work.
- Use idempotency keys or deterministic operation identities for retriable mutations.
- Persist intent and workflow phase before executing external side effects where the spec requires recovery.
- Define reverse-order compensation for every cross-service mutation.
- Treat timeouts as unknown outcomes, not automatic failures.
- Observe before retrying destructive or duplicating mutations.
- Avoid `unsafe`; each use requires a dedicated ADR, safety comments, tests, and reviewer approval.
- Avoid global mutable state.
- Never log secrets, tokens, image credentials, private keys, connection information, user-data, connector data, or unredacted provider payloads.
- Every background task must have shutdown, retry, backoff, reconnect, and observability behavior.
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
- provider or external-service fake and conformance tests;
- cross-service compensation tests;
- failure-path tests;
- regression tests for bugs;
- process-level tests;
- component, hosted-service, or full-cloud integration tests when required by the evidence tier.

Mocks that only repeat implementation expectations are not evidence. Prefer stateful fakes, real databases, real processes, and black-box HTTP tests. Do not mock away the exact execution boundary that an issue is supposed to prove.

## Commands required before completion

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Run additional contract, simulated-cloud, process, component, hosted-service, or integration commands named by the issue. Report commands and results in the PR.

## PR response format

Every agent-authored PR description must include:

- **Issue:** linked issue;
- **Intent:** one paragraph;
- **Design:** important choices and rejected alternatives;
- **Profile:** service ownership, operations, and evidence tier affected;
- **Contracts:** changed or unchanged;
- **Workflow:** dependencies, phases, and compensation affected;
- **Tests:** exact commands and important cases;
- **Evidence:** portable, component, hosted-service, full-cloud, or release status;
- **Risks:** unresolved concerns;
- **Provenance:** public sources and AI tools used;
- **Follow-ups:** separate coherent issues, not hidden TODOs or field-level micro-issues.

When the public Go O3K repository is consulted, the PR must additionally record:

- the repository URL and exact commit;
- Go files, handlers, routes, tests, or fixtures inspected;
- whether any artifact was copied or adapted (normally none);
- Apache-2.0 attribution and NOTICE handling for every reused artifact.

Go behavior is requirements and comparison evidence only. Official OpenStack specifications and public client behavior remain normative, and mechanical translation or reproduction of Go architecture is prohibited unless a separate accepted decision explicitly authorizes it.

## Definition of done

- acceptance criteria satisfied;
- compatibility profile, service ownership, and evidence tier identified;
- tests fail before/fix after when practical;
- formatting, lint, unit, and relevant integration tests pass;
- contracts, traceability, and documentation updated;
- no unrelated change;
- no unexplained TODO, panic, `unwrap`, or ignored error in production paths;
- compatibility evidence updated when external behavior changes;
- no release or catalog claim exceeds its evidence;
- reviewer can understand the change without reconstructing agent reasoning.
