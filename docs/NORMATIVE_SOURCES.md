# Normative source ownership

This document prevents architectural and compatibility rules from being copied
into multiple summaries and drifting independently.

## Authority map

| Subject | Normative source | Summary-only documents |
|---|---|---|
| Service topology and process extraction | `docs/adr/ADR-0160-service-topology-and-execution-boundaries.md` | `docs/ARCHITECTURE.md`, `docs/ROADMAP.md`, `docs/PROJECT_CHARTER.md` |
| Keystone trust, identity, catalog, and `AuthContext` | `docs/specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md` | `docs/ARCHITECTURE.md`, `docs/PRODUCT_REQUIREMENTS.md` |
| Cross-service workflows and compensation | `docs/specs/SPEC-0021-cross-service-workflows-and-compensation.md` | `docs/ARCHITECTURE.md`, `docs/TEST_STRATEGY.md` |
| Declared API operations and evidence gates | `docs/specs/SPEC-0022-service-api-baseline-and-evidence-gates.md` plus machine-readable compatibility manifests | `docs/PRODUCT_REQUIREMENTS.md`, `docs/ROADMAP.md` |
| External OpenStack service-under-test profile | `docs/specs/SPEC-0023-external-cinder-service-under-test.md` | `docs/ARCHITECTURE.md`, `docs/PROJECT_CHARTER.md`, `docs/ROADMAP.md` |
| Compute/network/storage execution authority and protocol invariants | `contracts/execution-boundaries.md` and accepted protobuf contracts | `docs/ARCHITECTURE.md`, `AGENTS.md` |
| LLM/coding workflow | `AGENTS.md`, `docs/LLM_DEVELOPMENT.md`, and accepted issue scope | PR templates and examples |

## Rules

- Summary documents explain intent and link to the normative source. They do
  not redefine field-level contracts, retry state machines, compensation order,
  or authorization semantics.
- When summary text conflicts with a normative source, the normative source
  wins and the summary must be corrected.
- Runtime behavior and release claims still require executable evidence. A
  normative document is not proof that an implementation exists.
- Proposed ADRs and specs guide implementation only when an accepted issue or
  PR explicitly adopts them. Merging a decision PR must set the final decision
  status according to the repository ADR lifecycle.
- Every compatibility claim must distinguish the upstream reference version,
  the O3K-advertised range, the implemented range, and the verified range.
- External-hosted services must not be presented as O3K-implemented services.

## Change discipline

Before changing a rule, edit its normative source first. Update summaries only
to preserve readability and links. Do not copy a complete workflow or contract
into `AGENTS.md`, the charter, roadmap, architecture overview, and a spec.
