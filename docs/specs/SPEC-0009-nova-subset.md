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

The Nova keypair subset supports public-key import and project/user-scoped
list, show, and delete operations under `/os-keypairs`. Keypair records are
durable SQLite state, names are unique within an authenticated user/project
scope, and fingerprints are computed from the decoded public-key blob. The
profile intentionally does not generate or persist private keys. A server
create may reference an owned `key_name`; the association is persisted and
returned, but guest key injection remains outside this profile until the
config-drive issue is complete.

The optional Nova `config_drive` request field is recognized. `false` is
accepted as an explicit no-op in this profile; `true` is rejected with `400`
before lifecycle intent is persisted because this profile does not yet
materialize or attach config-drive media. The API does not silently ignore a
request for config-drive data.

Observed powered-off instances are exposed with Nova's `SHUTOFF` status. The
provider's internal `Stopped` state is not exposed as the non-Nova `STOPPED`
string.

Supported actions are start, stop, and reboot. Delete is idempotent after the
server has reached the deleted projection. Metadata, resize,
rebuild, rescue, pagination, quotas, full microversion coverage, and provider
network attachment are intentionally out of scope for this alpha slice.
Keypair private-key generation and guest `authorized_keys` injection are also
out of scope.
