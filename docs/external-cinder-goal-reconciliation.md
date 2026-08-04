# External Cinder Service-Testbed Goal — Acceptance Reconciliation

Status: Working

This document records an evidence-based reconciliation of the external Cinder
service-testbed goal against the original acceptance criteria of issues #420,
#421, #424, #429, and #432, and the merged partial implementations PRs #449,
#450, #451, and #452.

It is the authority for issue re-opening and for the dependency order of the
remaining work. It does not reinterpret the original issue scope to match the
existing implementation.

## Method

Each issue was compared against:

- its original body and acceptance criteria (quoted from the issue);
- the merged PRs that claimed to close it;
- the current `main` branch source, migrations, tests, workflows, and docs;
- the normative documents SPEC-0020, SPEC-0021, SPEC-0022, SPEC-0023, SPEC-0024,
  and ADR-0160/0161/0162/0163.

Evidence terms:

- `portable_evidence`: a process-level test that runs a compiled O3K binary
  against its own HTTP API without external services.
- `component_evidence`: a test of one O3K or external component against a fake,
  simulated, or real dependency, executed on a real host.
- `real_service_evidence`: a protected run against a real pinned external
  Cinder deployment with its own database, message bus, and backend.

---

## Issue #420 — Keystone hosted-service compatibility

### Original acceptance criteria

- an external Cinder service user can authenticate against O3K;
- token validation succeeds through the public Identity API;
- the returned catalog points `volumev3` to an explicitly registered external
  endpoint;
- unsupported services are omitted;
- cross-project and cross-service authorization tests fail closed;
- all identity state survives restart;
- no passwords, tokens, or private configuration appear in logs or artifacts;
- compatibility manifest and evidence are updated without claiming full
  Keystone support.

### Implemented (PR #449 + current `main`)

- `GET /v3/auth/tokens` token validation returning token details.
- `HEAD /v3/auth/tokens` token existence check.
- Single-user, single-project password authentication (`POST /v3/auth/tokens`)
  against a configured in-memory bootstrap identity.
- Optional `volumev3` catalog entry from `O3K_CINDER_ENDPOINT`.
- `AuthContext`, `ServiceRecord`, `EndpointRecord` domain types (unused by the
  API).
- Display-name vs durable-ID rejection for the bootstrap project.

### Partially implemented

- Durable store tables and methods for domains, projects, users, roles, role
  assignments, services, and endpoints exist in `o3k-store` (migration 0012,
  uncommitted as of this reconciliation) but are **not wired into the identity
  service**; the catalog is still generated entirely from in-memory config.
- Token signing is stateless HMAC; verification is hard-coded to the single
  bootstrap user/project.

### Missing

- durable domain/project/user/role/role-assignment/service/region/endpoint
  records created and consumed at runtime;
- a `service` project and dedicated Cinder service user with explicit roles;
- per-service endpoint registration (only `O3K_CINDER_ENDPOINT` bootstrap
  convenience exists);
- catalog generated from durable service and endpoint records;
- password hashing (passwords are compared in plaintext);
- restart-safe verification that revalidates user/project enabled state and
  re-derives roles from durable records;
- cross-project and cross-service authorization fail-closed tests against the
  public API;
- service-user authentication test;
- role-assignment enforcement tests;
- endpoint persistence across restart tests;
- invalid/expired/malformed/cross-project token tests;
- secret-redaction tests;
- compatibility-manifest records for the hosted-service identity surface.

### Evidence

```yaml
portable_evidence: keystone_get_and_head_token_validation_and_cinder_catalog (health.rs), o3k-identity unit tests, tests/portable-service-testbed-gate.sh
component_evidence: none beyond portable
real_service_evidence: none
correct_state: OPEN — durable identity and catalog are not implemented
```

---

## Issue #421 — Nova volume attachments and outbound Cinder client

### Original acceptance criteria

- declared subset of Nova `os-volume_attachments` routes with frozen
  methods/microversions/fields/policies in the compatibility manifest;
- typed bounded Cinder v3 client for the selected attachment sequence
  (authenticate service identity, create/reserve, provide connector, consume
  secret-safe connection info, complete, delete/terminate);
- no logging of connection information, tokens, credentials, or backend
  secrets;
- durable orchestration phases persisted before side effects with safe
  compensation;
- timeouts are unknown outcomes requiring observation before retry;
- required tests: public API contract, typed client against stateful fake,
  duplicate/idempotency, restart replay, project/service authorization, secret
  redaction, Cinder unavailable/timeouts/partial completion, compute attach
  succeeds but Cinder completion fails, detach and repeated detach, no orphan
  attachment or leaked device.

### Implemented (PR #450 + current `main`)

- Nova `os-volume_attachments` routes: create/list/show/delete.
- SQLite `volume_attachments` record with insert/list/get/delete store methods.
- Basic server-project validation and device-letter auto-assignment.

### Partially implemented

- The API is a record-only bridge: `attach_volume` writes an SQLite row; there
  is no outbound Cinder call, no connector acquisition, no compute dispatch,
  no phase machine, no compensation.
- The four handlers do not call `require_token`/`project_token`; only the
  compute service's `show_server` project check protects them.
- Attachment id is set equal to `volume_id`; the unique index on `volume_id`
  forbids multi-attach and conflates attachment identity with volume identity.

### Missing

- the typed outbound Cinder attachment client (none exists);
- durable phases `validated -> cinder_attachment_created ->
  connection_prepared -> compute_attached -> cinder_attachment_completed`
  with restart replay;
- detach phase model and repeated-detach idempotency;
- timeout-as-unknown-outcome semantics;
- compensation for every listed failure;
- secret-safe connection-info handling (receive, consume, redact, never
  persist raw);
- the full test matrix listed in the issue;
- compatibility-manifest freeze of the attachment operations.

### Evidence

```yaml
portable_evidence: nova_volume_attachment_lifecycle_list_create_show_delete (health.rs) — 404-only coverage
component_evidence: none
real_service_evidence: none
correct_state: OPEN — orchestration and the outbound client are not implemented
```

---

## Issue #424 — Focused Tempest and external-service compatibility gate

### Original acceptance criteria

- no test outside the declared profile is silently counted as supported;
- every skip has an explicit unsupported-operation record;
- real external Cinder can authenticate, validate tokens, discover its
  endpoint, access required image/compute surfaces, and complete the selected
  attachment workflow;
- failures identify the service boundary precisely;
- credentials and connection information are redacted;
- the gate is non-blocking for the first ephemeral-root alpha.

### Implemented (PR #451 + current `main`)

- `tests/portable-service-testbed-gate.sh` — curl black-box smoke of
  discovery, auth, token validation, catalog presence, microversion
  negotiation, Glance/Neutron list.
- Wired into `tests/compatibility-target.sh` (CI `ci.yml`).

### Partially implemented

- The gate is a portable smoke test; it is not a Tempest run, produces no
  JUnit artifacts, and defines no skip records.
- It sets `O3K_CINDER_ENDPOINT` to a port with no server and does not exercise
  any `os-volume_attachments` operation.

### Missing

- a pinned Tempest subset with explicit unsupported-operation skip mapping;
- machine-readable evidence (JUnit XML) and a human-readable profile report;
- stateful fake Cinder integration tests;
- real Cinder evidence (blocked on #420/#421/#429).

### Evidence

```yaml
portable_evidence: tests/portable-service-testbed-gate.sh, tests/compatibility-target.sh
component_evidence: none
real_service_evidence: none
correct_state: OPEN — no Tempest evidence and no real-service evidence
```

---

## Issue #429 — Real external Cinder service-under-test runner

### Original acceptance criteria

- the selected real Cinder version starts without DevStack or a full OpenStack
  control plane;
- Cinder's own database/message-bus/backend dependencies remain visible and
  documented;
- service-user auth, token validation, endpoint discovery, volume lifecycle,
  attach, detach, and cleanup pass;
- failures identify the exact O3K or external Cinder boundary;
- no connection information, token, password, or backend secret is uploaded;
- separate from the first ephemeral-root release gate.

### Implemented (PR #452 + current `main`)

- `scripts/external-cinder-testbed-runner.sh` — a Python `BaseHTTPRequestHandler`
  mock that answers `GET .../volumes` and validates the caller token through O3K
  Keystone, plus a `cinder-testbed.yml` workflow.
- o3kd launched with `O3K_CINDER_ENDPOINT` pointing at the mock.

### Partially implemented

- The runner is a **mock HTTP service**, not a real Cinder deployment. It
  provisions no database, no message bus, no `cinder-api`/`cinder-scheduler`/
  `cinder-volume`, and no storage backend.
- It exercises only token validation; there is no volume lifecycle, attachment,
  detach, or cleanup.

### Missing

- a real pinned Cinder deployment (database, message bus, api/scheduler/volume
  services, LVM backend);
- service-user registration and durable catalog registration in O3K Identity;
- volume create/show/delete and attachment lifecycle against real Cinder;
- compute-side attach and cleanup verification;
- secret-free evidence upload;
- explicit O3K-owned vs Cinder-owned vs backend ownership reporting.

### Evidence

```yaml
portable_evidence: none beyond token validation against the mock
component_evidence: scripts/external-cinder-testbed-runner.sh (mock only)
real_service_evidence: none
correct_state: OPEN — the "real Cinder" profile does not exist
```

---

## Issue #432 — Review follow-up tracker

### Original acceptance criteria

- groups the accepted review findings from PR #419;
- lists #420/#421/#422/#423/#424/#425/#429/#430/#431/#433/#435 as coherent
  follow-ups with a priority order;
- no field-level or duplicate micro-issues.

### Implemented

- The tracker body exists and is accurate about the follow-up set.

### Missing / needs update

- the tracker has no reconciled status per follow-up, no evidence references,
  no final closure rationale, and does not reflect which PRs partially
  addressed which issues.

### Correct state

```yaml
correct_state: OPEN — update with reconciled status and evidence references
```

---

## Dependency order for the remaining work

1. acceptance reconciliation and claim correction (this document);
2. durable hosted-service identity and catalog (#420);
3. typed outbound Cinder attachment client (#421);
4. durable attachment orchestration and compute block-device boundary (#421);
5. stateful fake Cinder failure/compensation tests (#421/#424);
6. real Cinder protected profile (#429);
7. focused Tempest evidence (#424);
8. final tracker and release-claim update (#432).

## Closure policy applied

No issue in this reconciliation is closed on the strength of API routes,
persisted records, a mock server, a green curl gate, or documentation alone.
Each issue remains open until every original acceptance criterion maps to an
implementation path, test, CI workflow, and evidence artifact.

## Provenance

- Issue bodies #420, #421, #424, #429, #432 (kubedoio/o3k-rust).
- PR bodies #449, #450, #451, #452 (kubedoio/o3k-rust).
- `main` at commit 107e96c plus the uncommitted store identity work on branch
  `issue-420-durable-keystone-identity`.
- No public Go O3K source was consulted for this reconciliation.

---

## Current status — real Gazpacho Cinder service-testbed (2026-08-04)

PRs #454–#459 landed on `main` for the real-service profile. The protected
workflow, disposable run-ID resources, pre/post-run guards, evidence artifacts,
and the real Nova-to-compute attachment integration are in place. The real
compute path (libvirt lifecycle + mTLS) is proven locally (PR #456 evidence).

### Real-service run findings (defects blocking #429 closure)

A real run of `scripts/real-cinder-testbed-runner.sh` against Cinder 28.0.0
(PyPI venv, MariaDB, RabbitMQ, LVM/iSCSI) progresses through identity and
cinder-api startup but fails at volume creation. Defects identified by
execution (not guesswork):

1. **Fixed — `python-memcached` missing** from the Cinder venv. Cinder's
   keystonemiddleware token-cache pool requires the `memcache` module; without
   it `cinder-api` returns HTTP 500 on every request. Re-applied in PR #460
   (the earlier commit was never merged).
2. **Open — RabbitMQ `ACCESS_REFUSED`** for the run-owned user from the Cinder
   scheduler/volume services. The run-owned user/vhost are created before the
   services start; the refusal indicates the services resolve a different
   credential or the user creation raced. Needs a targeted reproduction.
3. **Open — keystonemiddleware `EndpointNotFound`** during token verification
   on the volume-create path. O3K's catalog advertises `identity` at
   `{base}/v3` (validated by the portable test and the admin-token catalog),
   so the middleware config/interface negotiation needs investigation against
   real Cinder. This is the Phase-6 identity-compatibility gate.

The volume-service also reports the run-owned LVM VG "not found" from the
`cinder-volume` process; the VG exists on the host, so this points to
rootwrap/privilege or VG-activation handling for the venv-launched volume
service.

These are the precise items the deferred external-Cinder work must resolve
before #420/#421/#424/#429 can close.
