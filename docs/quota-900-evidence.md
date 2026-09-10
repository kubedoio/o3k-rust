# #900 native quota evidence

Base: `ed0648e799b23e619a27d5ec94cee3dec6108393` (protected `main`).
Original PR head: `b488de655e40763542bc0c483ac8021ebec844f9`.
Current implementation head: `90d3a440` (this revision).

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
The PostgreSQL conformance test was run against disposable PostgreSQL 16 in a
localhost container: independent writers had one success/one stale conflict,
restart preserved generation, and the audit migration/conformance suite passed.
No adapter-local generation lock or shadow quota state remains.

The production-composition HTTP journey now mounts `/o3k/v1/quota` and the
operator quota routes, reads tenant-scoped dimensions and usage, observes
usage after native Compute allocation and release, and rejects a tenant's
foreign operator read. `network:address_allocations` usage is backed by the
canonical allocation table in both stores.

The complete local workspace gates and focused SQLite/PostgreSQL process
journeys pass. GitHub CI for this revision is the final external gate; the
preceding revision's failure was isolated to a concurrent PostgreSQL audit
migration fixture (`VersionMissing(25)`) and is being rechecked on this
revision.
