# #900 native quota evidence

Base: `ed0648e799b23e619a27d5ec94cee3dec6108393` (protected `main`).
Original PR head: `b488de655e40763542bc0c483ac8021ebec844f9`.
Current implementation head: `4938ce8ff2d60d8f3b2596212f23db9410f3f7df`.

The implementation projects the existing SQLite/PostgreSQL `QuotaRepository`
and SQL usage counters through the versioned native `/quota` contract. It does
not count resources in the HTTP layer and does not expose reservations. Tenant
scope is derived exclusively from `AuthContext`; operator reads/writes use the
canonical `quota:ReadQuota` and `quota:ManageQuota` policies. Limit and audit
event are committed atomically by the durable repository transaction.

Durable generation is initialized to `0` (including migrated rows) and is
advanced exactly once by an atomic compare-and-set. Clear/reset is an explicit
`Unlimited` replacement and requires `expected_generation`. SQLite coverage
includes two independent store instances and stale-writer rejection; the
PostgreSQL conformance test is gated on `O3K_DATABASE_URL` and must run against
the repository's real PostgreSQL service. No adapter-local generation lock or
shadow quota state remains.

At this checkpoint, package compilation and the SQLite CAS regression pass.
Full workspace gates, real PostgreSQL execution, and the real `o3kd` process
journey must be recorded from CI/runtime before this evidence can claim READY.
