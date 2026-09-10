# #900 native quota evidence

Base: `ed0648e799b23e619a27d5ec94cee3dec6108393` (protected `main`).

The implementation projects the existing SQLite/PostgreSQL `QuotaRepository`
and SQL usage counters through the versioned native `/quota` contract. It does
not count resources in the HTTP layer and does not expose reservations. Tenant
scope is derived exclusively from `AuthContext`; operator reads/writes use the
canonical `quota:ReadQuota` and `quota:ManageQuota` policies. Administrative
writes publish through the production `RequiredAuditPublisher`.

Validation run for this change: `cargo check -p o3k-native-api -p o3kd`.
Full workspace gates and backend/process evidence remain required before merge.
