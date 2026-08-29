# Refactor Contract — Issue #758

## Purpose

Every PR that claims to be a "pure refactor" (no behavior change) must answer
every question below **NO** before merging. If any answer is **YES**, the PR
must not be labelled or described as a pure refactor; it requires a separate
accepted specification or architecture decision.

## Checklist

### Public API / Compatibility

| # | Question | Answer | Notes |
|---|----------|--------|-------|
| 1 | Public/native API semantics changed | NO | |
| 2 | OpenStack compatibility behavior changed | NO | |
| 3 | Wire/protocol contracts changed | NO | |
| 4 | Authorization/ownership/tenant isolation changed | NO | |
| 5 | Audit/correlation identity changed | NO | |

### Database / Persistence

| # | Question | Answer | Notes |
|---|----------|--------|-------|
| 6 | Database schema/migrations changed | NO | |
| 7 | SQL semantics changed | NO | |
| 8 | Transaction boundaries changed | NO | |
| 9 | Lock ordering changed | NO | |
| 10 | SQLite/PostgreSQL parity changed | NO | |

### Provider / Execution

| # | Question | Answer | Notes |
|---|----------|--------|-------|
| 11 | Provider behavior changed | NO | |
| 12 | Host mutation behavior changed | NO | |
| 13 | Idempotency/replay identity changed | NO | |
| 14 | Unknown-outcome behavior changed | NO | |
| 15 | Controller/session/agent/generation fencing changed | NO | |

### Recovery

| # | Question | Answer | Notes |
|---|----------|--------|-------|
| 16 | Recovery/compensation behavior changed | NO | |

### Conformance / Evidence

| # | Question | Answer | Notes |
|---|----------|--------|-------|
| 17 | P12/P13 conformance/evidence semantics changed | NO | |
| 18 | Accepted ADR/SPEC semantics changed | NO | |

## Enforcement

Before merge, the PR author must:

1. Copy this checklist into the PR description.
2. For every YES answer, link to the accepted spec/ADR that authorizes the
   change and explain why the change is not a pure refactor.
3. Run the full validation gate (`cargo fmt`, `clippy`, `test`, nextest).
4. Confirm the P13.4 provider lifecycle tests pass with the exact pinned
   provider binary (terraform-provider-openstack 3.4.0, SHA-256
   `2840ef5e25598f85591cf984825a8a19b9de498782cfe253e6d3e78740fbd5dc`).

## Exceptions

No exceptions are granted by this document. Any YES answer requires explicit
architecture review approval.
