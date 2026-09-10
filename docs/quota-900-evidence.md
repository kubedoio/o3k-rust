# #900 native quota evidence

Base: `ed0648e799b23e619a27d5ec94cee3dec6108393` (protected `main`).
Original PR head: `b488de655e40763542bc0c483ac8021ebec844f9`.
Evidence implementation revision: `78945f354e475874a3c49b3a1b4068b3243bba57` (final reviewed branch head).

The implementation projects the existing SQLite/PostgreSQL `QuotaRepository`
and SQL usage counters through the versioned native `/quota` contract. It does
not count resources in the HTTP layer and does not expose reservations. Tenant
scope is derived exclusively from `AuthContext`; operator reads/writes use the
canonical `quota:ReadQuota` and `quota:ManageQuota` policies. Limit and audit
event are committed atomically by the durable repository transaction.

Durable generation is initialized to `0` (including migrated rows) and is
advanced exactly once by an atomic compare-and-set. Clear/reset is an explicit
`Unlimited` replacement and requires `expected_generation`. SQLite coverage
includes two independent store instances, stale-writer rejection, and a
database-trigger failure proving quota and required Audit rollback together.
The PostgreSQL conformance test was run against a disposable role and database
on the local PostgreSQL 16.15 daemon (localhost only): independent writers had one success/one stale conflict,
restart preserved generation, and the audit migration/conformance suite passed.
No adapter-local generation lock or shadow quota state remains.

The production-composition HTTP journey now mounts `/o3k/v1/quota` and the
operator quota routes, reads tenant-scoped dimensions and usage, observes
usage after native Compute allocation and release, and rejects a tenant's
foreign operator read. The delegated file-backed compatibility floating-IP
allocator is intentionally not advertised as a native quota dimension because
its allocation and quota state are not one transactional authority; native
quota dimensions are limited to dimensions enforced by the Cloud Kernel.

The real OIDC-backed process journey also exchanges a system/operator
credential through `/o3k/v1/identity/tokens`, reads a foreign project's quota,
sets a finite Compute limit with the generation precondition, observes the
durable Audit-backed generation advance, rejects a stale reset, and verifies a
second tenant allocation is rejected before the provider is touched.

Compute create failure and compensation paths release pending quota
reservations, and terminal `ERROR` compute resources are excluded from active
usage counters. The regression is covered by
`failed_create_releases_quota_reservation`.

The complete local workspace gates and focused SQLite/PostgreSQL process
journeys pass. GitHub CI for the final pushed revision is the external gate;
the earlier PostgreSQL audit migration fixture failure (`VersionMissing(25)`)
was isolated to concurrent shared-fixture setup and is covered by the
disposable-database conformance path.
