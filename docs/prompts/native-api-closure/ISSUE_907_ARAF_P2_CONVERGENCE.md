# ISSUE 907 — Araf P2 Northbound Contract Convergence Gate

## Authority

- Issue: `#907 — [Program] Production northbound contract closure for Araf P2`
- PR: `#907 convergence gate` on branch `feat/araf-p2-convergence-907`
- Baseline main: `ad6d4b096ae8b917e16e3a4fc98037f3b10385e9` (post-#904)

## What this workstream is

This is NOT a feature workstream. Every #887–#906 contract is already merged
into main. The purpose here is to prove, in ONE supported production-profile
environment, that all of them converge correctly together, and to fix any
genuine integration/correctness defect the gate discovers.

When this gate is complete and independently reviewed, Araf must be able to
proceed through P2.2–P2.8 without any CURRENTLY KNOWN missing O3K northbound
contract in the agreed scope.

## Non-goals

- No Araf work.
- No pricing/billing, DNS, LBaaS, object storage, Kubernetes-as-a-Service,
  DBaaS, marketplace, organization hierarchy, or other future product domains.
- No new product APIs invented to make the convergence test easy.
- No weakening of security; no bypassing production routing/authentication.

## Merged contracts being converged (all on main)

| Workstream | Contract | Where |
|---|---|---|
| #887 regions/AZ | ADR-0181 / SPEC-0038 | `GET /o3k/v1/regions`, `contracts/native-location-discovery-v1.schema.json` |
| #888 schemas/actions | ADR-0173+0174 / SPEC-0040, SPEC-0030 | `GET /o3k/v1/services`, `/resource-types`, `/resource-schemas/{ns}/{c}/{v}` |
| #897 domain actions | SPEC-0030/0040 | `POST /o3k/v1/{ns}/{c}/{id}/actions/{action}` |
| #898 Operation collection | SPEC-0008 | `GET /o3k/v1/operations` |
| #899 relationships | SPEC-0030 | `GET /o3k/v1/{ns}/{c}/{id}/relationships` |
| #900 quota | SPEC-0043 | `/quota`, `/operator/quotas/...` |
| #901 IAM governance | SPEC-0044 | `/operator/governance/...` |
| #902 durable audit | SPEC-0042 | `GET /o3k/v1/audit` |
| #903 diagnostics | SPEC-0045 | `/operator/diagnostics*` |
| #904 metering | ADR-0183 / SPEC-0046 | `/metering/definitions`, `/metering/usage` |
| #905 lifecycle update | SPEC-0030 | `PUT /o3k/v1/{ns}/{c}/{id}` |
| #906 bounded queries | SPEC-0030/0008 | repository-level pagination on all collections |

## Gate architecture

One integrated environment, real everything:

- Real `o3kd` binary (spawned as a subprocess on an ephemeral bound port via
  `CARGO_BIN_EXE_o3kd`), not handler-instantiation.
- Real production router (`o3k_api::router_with_state` via `bins/o3kd`).
- Real authorization: Keycloak 25.0.6 (pinned, disposable docker fixture)
  issues RS256 OIDC tokens; the production `OidcValidator`/federated scope
  exchange (`POST /o3k/v1/identity/tokens`) maps them to canonical O3K
  `AuthContext`. No injected AuthContext, no unsigned JWTs.
- Real repositories: disposable PostgreSQL 16.4 (primary production profile)
  plus a SQLite parity pass.
- Real provider boundary: `O3K_PROVIDER=fake` compute provider and in-memory
  storage provider — the accepted execution-boundary fixtures; the gate must
  not bypass the adapter/router/auth layers.

## The 16 required integrated evidence items

1. **Federated login** — OIDC discovery → issuer validation → RS256 token →
   `POST /o3k/v1/identity/tokens` scope exchange → project-scoped AuthContext
   for Tenant A and Tenant B; operator identity bound through canonical
   durable IAM (`operator-assignments`).
2. **Discovery** — services, resource types, action/create schemas, regions/AZ
   all served from O3K only; advertised capabilities must be executable.
3. **Generic lifecycle** — bounded list/show/create/update/delete through
   declared native contracts with idempotency-key replay.
4. **Domain actions** — compute start/stop/reboot via discovered action
   schema, canonical Operation semantics, replay-safe.
5. **Operations** — mutation → Operation returned → discoverable in bounded
   collection → show/list agree → restart preserves truth; cross-tenant
   concealment.
6. **Relationships** — compute→network (and volume where the active profile
   supports it) canonical relationship visibility, bounded projection, no
   provider-private IDs, restart reconstruction, delete semantics.
7. **Quota** — operator sets finite limit → tenant allocates to the limit →
   rejection without provider side effect or reservation leak → release
   frees capacity → stale-generation mutation rejected → tenant cannot
   self-administer or read foreign quota.
8. **Governance/IAM** — operator lists projects/principals/roles, grants and
   revokes project membership with refreshed AuthContext reflecting the
   change; privilege-escalation negatives (tenant self-grant, project-admin
   manufacturing system scope, claim injection).
9. **Audit** — creation/action/quota/governance mutations produce durable
   audit records surviving restart, bounded query, canonical actor/scope/
   target, no secrets.
10. **Diagnostics** — operator sees service/provider/location health and
    VCPU/MEMORY_MB/DISK_GB capacity with freshness semantics; a controlled
    degradation shows stale/degraded/unavailable then recovers; tenant and
    project-scoped operator-like identities denied; no private leakage.
11. **Metering** — `compute:instance_seconds` over a deterministic
    run/stop/start/delete journey matches SPEC-0046 exactly; completeness,
    `authority_started_at`, `observed_through` correct; restart/replay
    produce no duplicate folding; cross-scope reads require canonical
    authority. (`volume:allocated_byte_seconds` is advertised only when a
    native storage provider exists — the fake profile must assert honest
    non-advertisement instead of fabricating it.)
12. **Cross-tenant isolation matrix** — Project B cannot read/action/list
    Project A resources, relationships, Operations, Audit, quota, usage, or
    governance; ID probes, list filters, cursor reuse, action routes, and
    usage paths do not become existence oracles beyond accepted semantics.
13. **Secret leak scan** — every native 2xx and representative error payload
    in the journey is scanned for passwords, tokens, client secrets, provider
    credentials, private keys, DB URLs, internal paths, agent epochs, and
    secret-bearing errors; structural DTO assertions where possible.
14. **Restart/recovery** — deliberate `o3kd` restart mid-journey against the
    same PostgreSQL database preserves resources, relationships, Operations,
    quota, IAM assignments, Audit, and metering; ephemeral diagnostics state
    returns unknown/stale honestly instead of fabricated health.
15. **Bounded queries** — representative collections (resources, Operations,
    Audit, metering) demonstrate repository-level bounds: max page, cursor
    continuation, invalid/tampered cursor rejection, filter-bound cursor
    mismatch, maximum range/meters/buckets.
16. **Compatibility convergence** — selected OpenStack-compatible surface
    (Nova servers, Neutron network view, Keystone auth) sees the same
    canonical resources created natively; existing Tempest/Mock Cinder gates
    remain green.

## Execution rules

- Reuse `tests/p12-iam-7-real-idp.sh` Keycloak provisioning pattern and
  `scripts/test-postgres-audit.sh` disposable-Postgres pattern.
- Never commit credentials; provision disposable infrastructure and clean it
  up (`trap ... EXIT`).
- The orchestrator is `tests/araf-p2-convergence.sh`; the in-tree Rust gate
  is `bins/o3kd/tests/araf_p2_convergence.rs` (spawns the real binary).
- A missing test is classified as missing evidence, not missing product
  functionality; a genuine integration defect found by the gate is fixed in
  the product, not worked around in the gate.
- Run all repository validation gates before completion.
