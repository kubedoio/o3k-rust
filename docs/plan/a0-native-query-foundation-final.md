# A0 native query and discovery foundation — execution plan

- Issue: #906 (A0 portion of #907).
- Deployment/evidence profile: native O3K portable/TestLab plus PostgreSQL
  persistence conformance; no production-readiness or HA claim.
- Canonical domain: Cloud Kernel resource registry and native resource query
  application boundary.
- Compatibility adapters: existing selected OpenStack adapters remain unchanged;
  native reads must converge on the same durable resource authority.
- Authority mode: `o3k-implemented`; `AuthContext` supplies effective scope and
  repositories own durable resource truth.
- Affected packages: `o3k-native-api`, `o3k-store`, `o3kd`, and native router
  composition in `o3k-api`.
- Expected files/symbols: native `ResourceQuery`, `ResourcePage`, cursor service,
  repository page ports/SQLite/PostgreSQL implementations, generic native
  resource adapter, live dispatcher readiness/capability projection, A0 verifier.
- Normative sources consulted: ADR-0173, ADR-0174, ADR-0181, SPEC-0030,
  SPEC-0031, SPEC-0038, and core architecture boundaries.
- Contract impact: SPEC-0030 pagination/discovery semantics and checked-in A0
  collection inventory; no A1 relationships/Operations or A2 mutation contract.
- Public operation: declared native List only. Scope is never caller selected.
- Database assumptions: stable `id ASC` keyset pagination; SQLite/PostgreSQL
  parity; stores materialize at most requested `N + 1` rows.
- Cross-service dependencies: live manifest/controller readiness and each
  resource application's declared bounded collection support; no compensation
  because A0 is read-only.
- Evidence tier: contract, architecture, store parity, native router, large-page
  instrumentation, and portable compatibility regression.
- Tests first: strict limit/cursor validation, scope/query cursor isolation,
  typed page invariants, store fetch counters, capability/readiness transitions,
  mutation-between-page behavior, verifier self-failure cases.
- Uncertainties: some manifest-declared collections may lack a bounded authority;
  those capabilities must be suppressed through runtime support, not names.
- Non-goals: Update, domain actions, relationship projection, Operation
  collection, governance, quota, Audit, diagnostics, or Metering.

Scope widens only when compilation or a falsifying test proves a production
native collection reaches an unclassified adapter/store boundary.
