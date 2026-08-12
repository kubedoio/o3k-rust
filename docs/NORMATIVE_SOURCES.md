# Normative source ownership

This document prevents architectural, product, compatibility, and security rules
from being copied into multiple summaries and drifting independently.

## Authority map

| Subject | Normative source | Summary-only documents |
|---|---|---|
| O3K product identity as a Cloud Operating System, Cloud Kernel authority, OpenStack-as-compatibility, provider versus delegated-cloud authority | `docs/adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md` | `README.md`, `docs/PROJECT_CHARTER.md`, `docs/PRODUCT_REQUIREMENTS.md`, `docs/ROADMAP.md`, `docs/ARCHITECTURE.md`, architecture visuals |
| O3K IAM authority, authorization model, service identity, Keystone compatibility boundary | `docs/adr/ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md` and `docs/specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md` | `README.md`, `docs/ARCHITECTURE.md`, `docs/PROJECT_CHARTER.md`, `docs/PRODUCT_REQUIREMENTS.md` |
| Deployment/evidence profiles, database posture, footprint claims, and profile-specific release claims | `docs/adr/ADR-0163-product-profiles-and-deployment-posture.md`, `docs/specs/SPEC-0024-product-profiles-and-claims.md`, and `compatibility/product-profiles.yaml` | `README.md`, `docs/PROJECT_CHARTER.md`, `docs/PRODUCT_REQUIREMENTS.md`, `docs/ROADMAP.md`, `docs/ARCHITECTURE.md` |
| Service topology, inward dependency direction, persistence authority, process/crate extraction, and Rust rewrite convergence | `docs/adr/ADR-0160-service-topology-and-execution-boundaries.md`, `docs/specs/SPEC-0025-rust-rewrite-and-architecture-convergence.md`, and `contracts/core-architecture-boundaries.toml` | `README.md`, `docs/ARCHITECTURE.md`, `docs/ROADMAP.md`, `docs/PROJECT_CHARTER.md` |
| Cross-service workflows and compensation | `docs/specs/SPEC-0021-cross-service-workflows-and-compensation.md` | `docs/ARCHITECTURE.md`, `docs/TEST_STRATEGY.md` |
| Declared OpenStack compatibility operations and evidence gates | `docs/specs/SPEC-0022-service-api-baseline-and-evidence-gates.md` plus machine-readable compatibility manifests | `README.md`, `docs/PRODUCT_REQUIREMENTS.md`, `docs/ROADMAP.md` |
| External OpenStack service-under-test profile | `docs/specs/SPEC-0023-external-cinder-service-under-test.md` | `README.md`, `docs/ARCHITECTURE.md`, `docs/PROJECT_CHARTER.md`, `docs/ROADMAP.md` |
| Compute/network/storage execution authority and protocol invariants | `contracts/execution-boundaries.md` and accepted protobuf contracts | `docs/ARCHITECTURE.md`, `AGENTS.md` |
| LLM/coding workflow and profile-selection rules | `AGENTS.md`, `docs/LLM_DEVELOPMENT.md`, accepted ADRs/specs, and accepted issue scope | PR templates and examples |

## Core architectural rules

### O3K is the cloud authority for O3K-owned resources

For O3K-owned resource profiles, O3K owns:

- public O3K identity;
- resource ownership/security scope;
- desired state;
- operation identity;
- scheduling/capacity decisions;
- compensation/reconciliation;
- compatibility projection;
- provider mappings.

Execution agents/providers own bounded mutations and observations only.

### OpenStack is a compatibility surface

Keystone, Nova, Neutron, Glance, Placement, and Cinder names are public
compatibility concepts where selected. They do not automatically define O3K
crate, process, persistence, or domain boundaries.

OpenStack JSON, headers, microversions, policy names, and catalog shapes are edge
representations.

The canonical O3K domain must remain independent from those wire models.

### O3K IAM is canonical

Keystone-compatible authentication/catalog behavior maps into the O3K IAM model.

First-class services consume one canonical `AuthContext` and common
principal/action/resource/ownership authorization contract.

A new O3K service must not invent an independent tenant-isolation model or parse
Keystone credentials as domain state.

### Existing clouds are not execution providers by default

A cloud that already owns scheduling, policy, quotas, resource identity, or
lifecycle is a delegated/federated control plane, not merely a libvirt-like
execution backend.

A generic cross-cloud provider abstraction is forbidden without a separate
accepted authority model.

## General rules

- Summary documents explain intent and link to normative sources. They do not
  redefine field-level contracts, retry state machines, compensation order,
  authorization semantics, product-profile gates, or claim states.
- When summary text conflicts with a normative source, the normative source
  wins and the summary must be corrected.
- Runtime behavior and release claims require executable evidence. A normative
  document is not proof that an implementation exists.
- Accepted ADRs define architecture. Proposed/superseded ADRs are not stronger
  than the accepted decision that supersedes them.
- Every OpenStack compatibility claim distinguishes upstream reference,
  O3K-advertised range, implemented range, and verified range.
- Every release/product claim identifies the deployment/evidence profile to
  which it applies.
- External-hosted services must not be presented as O3K-implemented services.
- PostgreSQL must not be described as supported until an adapter and conformance
  evidence exist.
- An approximately 50 MB footprint must remain a target until a
  profile-specific measurement artifact exists.
- "Connect to another OpenStack" must be decomposed into explicit trust,
  hosted-service, external-identity, endpoint-registration, service-consumption,
  delegation/federation, or resource-sharing profiles.
- Go O3K is a behavioral/reference input, not the Rust architecture authority.
  Route-count parity, package parity, database-schema parity, and mechanical
  translation are not rewrite goals.
- The core domain owns canonical O3K identities and lifecycle invariants.
  Public API, SQL, provider, protobuf, libvirt, and external-service models are
  edge representations.
- Application services depend on narrow ports rather than concrete persistence
  or execution adapters. Temporary architecture debt must be named in
  `contracts/core-architecture-boundaries.toml`; the debt set may shrink but
  must not grow without explicit architecture review.
- Filesystem/blob storage may own bounded bytes and host-local artifacts, but
  must not silently become the only source of public control-plane identity,
  ownership, desired state, allocation intent, or recovery state.
- The native ephemeral-root TestLab remains the first-alpha release-blocking
  path. Cloud Kernel expansion and external-service work are non-blocking unless
  a later accepted human decision changes that gate.
- "Cloud Operating System" is an architecture/product-direction statement.
  Production, HA, parity, database, footprint, or service-breadth claims remain
  evidence-gated.

## Change discipline

Before changing a rule, edit its normative source first. Update summaries only
to preserve readability and links.

Do not copy a complete workflow or contract into `AGENTS.md`, the charter,
roadmap, architecture overview, and a spec.

Architecture-boundary exceptions are not ordinary implementation details. When
one is required, update the normative ADR/spec and machine-readable boundary
contract in the same reviewed change, explain why inward dependency direction
cannot yet be preserved, and identify the coherent removal follow-up.
