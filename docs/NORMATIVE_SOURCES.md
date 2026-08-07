# Normative source ownership

This document prevents architectural, product, and compatibility rules from
being copied into multiple summaries and drifting independently.

## Authority map

| Subject | Normative source | Summary-only documents |
|---|---|---|
| Product profiles, database posture, footprint claims, and profile-specific release claims | `docs/adr/ADR-0163-product-profiles-and-deployment-posture.md`, `docs/specs/SPEC-0024-product-profiles-and-claims.md`, and `compatibility/product-profiles.yaml` | `README.md`, `docs/PROJECT_CHARTER.md`, `docs/PRODUCT_REQUIREMENTS.md`, `docs/ROADMAP.md`, `docs/ARCHITECTURE.md` |
| Service topology, inward dependency direction, persistence authority, process/crate extraction, and Rust rewrite architecture | `docs/adr/ADR-0160-service-topology-and-execution-boundaries.md`, `docs/specs/SPEC-0025-rust-rewrite-and-architecture-convergence.md`, and `contracts/core-architecture-boundaries.toml` | `README.md`, `docs/ARCHITECTURE.md`, `docs/ROADMAP.md`, `docs/PROJECT_CHARTER.md` |
| Keystone trust, identity, catalog, and `AuthContext` | `docs/specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md` | `docs/ARCHITECTURE.md`, `docs/PRODUCT_REQUIREMENTS.md` |
| Cross-service workflows and compensation | `docs/specs/SPEC-0021-cross-service-workflows-and-compensation.md` | `docs/ARCHITECTURE.md`, `docs/TEST_STRATEGY.md` |
| Declared API operations and evidence gates | `docs/specs/SPEC-0022-service-api-baseline-and-evidence-gates.md` plus machine-readable compatibility manifests | `README.md`, `docs/PRODUCT_REQUIREMENTS.md`, `docs/ROADMAP.md` |
| External OpenStack service-under-test profile | `docs/specs/SPEC-0023-external-cinder-service-under-test.md` | `README.md`, `docs/ARCHITECTURE.md`, `docs/PROJECT_CHARTER.md`, `docs/ROADMAP.md` |
| Compute/network/storage execution authority and protocol invariants | `contracts/execution-boundaries.md` and accepted protobuf contracts | `docs/ARCHITECTURE.md`, `AGENTS.md` |
| LLM/coding workflow and profile-selection rules | `AGENTS.md`, `docs/LLM_DEVELOPMENT.md`, accepted ADRs/specs, and accepted issue scope | PR templates and examples |

## Rules

- Summary documents explain intent and link to the normative source. They do
  not redefine field-level contracts, retry state machines, compensation order,
  authorization semantics, product-profile gates, or claim states.
- When summary text conflicts with a normative source, the normative source
  wins and the summary must be corrected.
- Runtime behavior and release claims require executable evidence. A normative
  document is not proof that an implementation exists.
- Proposed ADRs and specs guide implementation only when an accepted issue or
  PR explicitly adopts them. Merging a decision PR must set the final decision
  status according to the repository ADR lifecycle.
- Every compatibility claim distinguishes the upstream reference version, the
  O3K-advertised range, the implemented range, and the verified range.
- Every product claim identifies the product profile to which it applies.
- External-hosted services must not be presented as O3K-implemented services.
- PostgreSQL must not be described as supported until an adapter and conformance
  evidence exist.
- An approximately 50 MB footprint must be described as a target until a
  profile-specific measurement artifact exists.
- “Connect to another OpenStack” must be decomposed into an explicit hosted-
  service, external-identity, endpoint-registration, service-consumption,
  federation, or resource-sharing profile.
- Go O3K is a behavioral/reference input, not the Rust architecture authority.
  Route-count parity, package parity, database-schema parity, and mechanical
  translation are not rewrite goals.
- The core domain owns canonical O3K identities and lifecycle invariants.
  Public API, SQL, provider, protobuf, libvirt, and external-service models are
  edge representations and may not redefine those invariants independently.
- Application services depend on narrow ports rather than concrete persistence
  or execution adapters. Temporary architecture debt must be named in
  `contracts/core-architecture-boundaries.toml`; the debt set may shrink but
  must not grow without explicit architecture review.
- Filesystem/blob storage may own bounded bytes and host-local artifacts, but
  must not silently become the only source of public control-plane identity,
  project ownership, desired state, allocation intent, or recovery state.
- The native ephemeral-root TestLab remains the first-alpha release-blocking
  path. External-service-testbed work is parallel and non-blocking unless a
  later accepted human decision changes that gate.

## Change discipline

Before changing a rule, edit its normative source first. Update summaries only
to preserve readability and links. Do not copy a complete workflow or contract
into `AGENTS.md`, the charter, roadmap, architecture overview, and a spec.

Architecture-boundary exceptions are not ordinary implementation details. When
one is required, update the normative ADR/spec and machine-readable boundary
contract in the same reviewed change, explain why inward dependency direction
cannot yet be preserved, and identify the coherent removal follow-up.
