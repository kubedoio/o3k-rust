# O3K Rust

O3K Rust is a clean-slate, Apache-2.0, Rust-based control plane for lightweight OpenStack-compatible test environments, edge clouds, and small private clouds.

The project is owned and developed by Kubedo GmbH. It is not a source-code port of another O3K implementation. It starts from public OpenStack APIs, public standards, publicly documented cloud behavior, and operational lessons learned from building and running cloud, storage, virtualization, and Kubernetes systems.

> **Status:** bootstrap / pre-alpha. No production-readiness claim is made.

## Why this project exists

Traditional OpenStack deployments are powerful, but they can be too large and operationally expensive for integration tests, developer environments, edge sites, training labs, and smaller operators. O3K Rust explores a smaller model:

- enable only the components required by a scenario;
- install quickly on a single node, then grow to a small cluster;
- preserve useful OpenStack API compatibility;
- use reconciliation instead of fragile procedural orchestration;
- delegate VM, network, and storage execution to explicit providers;
- integrate naturally with CellHV without making CellHV mandatory;
- remain understandable enough for operators and LLM agents to reason about safely.

## Experienced ideas that guide the design

These are project principles derived from practical infrastructure experience. They are not copied implementation details.

1. **Deployment is part of the product.** A cloud that is difficult to install, reset, reproduce, or upgrade is not suitable for labs or small operators.
2. **Compatibility must be tested, not claimed.** Endpoint count is not a useful success metric. Real OpenStack CLI, SDK, Terraform, and contract behavior must pass reproducible tests.
3. **Start with complete workflows.** Token → image → network → flavor → server → delete is more valuable than hundreds of disconnected endpoints.
4. **Keep the default small.** The initial profile uses one process, SQLite, local image storage, a flat network, and a provider adapter. PostgreSQL, S3, clustering, and advanced networking are opt-in steps.
5. **Separate API semantics from infrastructure execution.** O3K owns OpenStack-facing resources and orchestration. Compute, storage, and network providers own execution.
6. **Desired state and observed state must converge.** Operations are idempotent, retryable, observable, and safe after process or node failure.
7. **CellHV is the preferred first real compute provider.** The integration uses a versioned contract. Neither repository imports the other's internal domain model.
8. **Failure paths are first-class features.** Restart, timeout, partial completion, duplicate request, stale state, rollback, and cleanup are tested before scale claims.
9. **Security and operations begin on day one.** Structured audit events, least privilege, secret boundaries, health checks, metrics, tracing, signed releases, and SBOMs are not deferred polish.
10. **A test environment is the first product.** Edge and SMB production profiles come only after the TestLab workflow is stable and measured.
11. **Rust is a means, not the product.** Rust is chosen for type safety, shared ecosystem with CellHV, reliable concurrency, and long-term maintainability—not as a substitute for correct cloud behavior.
12. **No hidden knowledge dependency.** Every implementation decision must be traceable to public specifications, a project ADR, an experiment, or an issue discussion.

## Initial product: O3K TestLab

The first vertical slice must support this workflow on a clean Linux host:

1. start O3K with SQLite;
2. obtain a Keystone-compatible token;
3. create or discover an image;
4. create a flat network and subnet;
5. create a flavor;
6. create a server through the stub provider, then CellHV;
7. list, inspect, stop, start, reboot, and delete the server;
8. restart O3K and reconcile state;
9. destroy and recreate the environment reproducibly.

### Initial non-goals

- full OpenStack replacement;
- all OpenStack extensions or microversions;
- billing and chargeback;
- live migration;
- distributed control-plane consensus;
- advanced Neutron services;
- Horizon feature parity;
- production multi-region operation;
- direct reuse or translation of non-public implementations.

## Architecture boundary

```text
OpenStack CLI / Terraform / SDKs
              |
        O3K Rust API
              |
  domain + policy + reconciliation
              |
      versioned provider contracts
        /          |          \
   compute       network      storage
     |              |            |
   CellHV        CellHV       CellHV/S3/local
```

O3K owns OpenStack-facing identity, catalog, image, network, compute, volume, placement, quota, policy, operation, and reconciliation semantics. Providers expose capabilities and execute bounded operations.

## Repository layout

```text
bins/o3kd/                 O3K server binary
crates/o3k-api/            HTTP routing and protocol adapters
crates/o3k-domain/         resource IDs, states, invariants, transitions
docs/                      product, architecture, ADRs, specs, agent rules
contracts/openapi/         public HTTP contracts
proto/provider/v1/         versioned provider contracts
.github/                   CI and contribution templates
```

The workspace will grow only when a spec and an accepted issue justify a new crate.

## LLM-first development model

This project is expected to be implemented primarily by LLM coding agents under human architectural review. LLM-first does **not** mean review-free or specification-free.

Every agent must:

1. read `AGENTS.md` and the relevant specs before changing code;
2. work from one issue with explicit acceptance criteria;
3. add or update tests before implementation is accepted;
4. keep changes small and auditable;
5. cite the public specification or ADR behind non-obvious behavior;
6. report uncertainty instead of inventing OpenStack semantics;
7. never copy non-public code or internal documents;
8. run formatting, linting, tests, and contract checks;
9. update compatibility and evidence documents when behavior changes.

See [AGENTS.md](AGENTS.md), [docs/LLM_DEVELOPMENT.md](docs/LLM_DEVELOPMENT.md), and [docs/CLEAN_IMPLEMENTATION.md](docs/CLEAN_IMPLEMENTATION.md).

## Build status

The bootstrap workspace provides a health endpoint and domain-state examples only. It intentionally does not claim OpenStack compatibility yet.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

## Licensing

O3K Rust is licensed under Apache-2.0. New dependencies must pass the project's license and provenance policy. The O3K name and Kubedo marks are not granted by the Apache-2.0 software license.
