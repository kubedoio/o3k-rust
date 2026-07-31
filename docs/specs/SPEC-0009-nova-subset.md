# SPEC-0009 — Nova TestLab lifecycle subset

Status: Implemented subset

O3K exposes `/v2.1/{project_id}` flavor and server routes for the TestLab
profile. The verified token, not the URL, supplies project ownership; a
mismatched project path is concealed as `404`.

The flavor catalog is a fixed read-only set (`test.small` and `test.medium`).
Server creation requires a name, image ID, flavor ID, and at least one network
reference. Creation journals the request in SQLite before invoking the fake
compute provider. The reconciler projects provider success to `ACTIVE` and
retains provider references for delete and action operations.

Observed powered-off instances are exposed with Nova's `SHUTOFF` status. The
provider's internal `Stopped` state is not exposed as the non-Nova `STOPPED`
string.

Supported actions are start, stop, and reboot. Delete is idempotent after the
server has reached the deleted projection. Keypairs, metadata, resize,
rebuild, rescue, pagination, quotas, full microversion coverage, and provider
network attachment are intentionally out of scope for this alpha slice.
