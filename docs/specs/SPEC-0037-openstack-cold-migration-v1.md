# SPEC-0037 — OpenStack Cold Migration v1

Status: Normative after ADR-0180 acceptance
Version: 1.0.0-architecture
Related issue: #856
Profile: `p14-openstack-cold-migration-v1`

## 1. Scope and authority

This specification defines a bounded, cold migration from a declared existing
OpenStack source into canonical O3K resources. It defines no runtime
implementation. OpenStack public APIs are read-only source authority until
cutover; O3K IAM, authorization, durable operations, resource ownership, and
destination APIs are canonical. Source database access and direct destination
database writes are forbidden.

The operation is authorized as:

`Principal × migrate:resource × destination scope/resource × AuthContext -> Allow | Deny`.

The original actor and authenticated migration service principal remain in
operation/audit context. A source credential is a capability, never an owner.

## 2. Durable contract

`Migration` has a stable ID, profile ID, generation, actor, source cloud ID,
destination scope, phase, source snapshot fingerprint, manifest digest,
cutover generation, timestamps, and failure/unknown-outcome detail. The phase
graph is:

```text
planned -> preflighted -> manifested -> transferring -> validated
         -> cutover-pending -> cutover-committed -> finalized
             |                    |
             v                    v
          blocked              rolled-back
```

Timeout or lost response enters `unknown-outcome` until observation establishes
the outcome. Retries use deterministic `(migration_id, generation, resource_key,
action)` idempotency identity and observe before duplicate-prone mutation.
Restart reconstructs work only from durable rows/events. Mapping uniqueness is
`(migration_id, source_resource_key)`; destination IDs are not reused after
compensation.

## 3. Manifest v1

```yaml
schema: o3k.migration.manifest/v1
migration_id: <opaque O3K ID>
profile_id: p14-openstack-cold-migration-v1
source: { cloud_id: <ID>, endpoint_fingerprint: <digest>, snapshot_fingerprint: <digest> }
destination: { profile: native-rust-testlab | small-edge-cloud, scope_id: <ID> }
actor: { principal_id: <ID>, authenticated_service_principal_id: <ID> }
nodes:
  - key: <source type>/<source ID>
    resource_type: <bounded type>
    source_id: <opaque ID>
    destination_id: <canonical ID or null>
    owner_scope_id: <canonical ID>
    depends_on: [<node key>]
    source_fingerprint: <digest>
    transfer: { mode: api | adapter, artifact_digest: <digest or null> }
    unsupported: []
cutover: { source_quiesced: false, committed_at: null }
integrity: { manifest_digest: <digest>, algorithm: sha256 }
```

Unknown fields are rejected or recorded as unsupported, never copied
implicitly. Secrets, tokens, private keys, passwords, cookies, and unredacted
provider payloads are excluded. A changed source snapshot requires a new
manifest generation or explicit operator restart.

## 4. Resource and transfer contract

The supported graph is:

`scope -> {image, flavor mapping, keypair public key, network, security group}
-> {subnet, router, port, router interface, floating/public IP}
-> {volume, volume attachment, server}`.

The source public API observes every node. The canonical O3K API/provider
boundary creates destination state. Images use authenticated byte transfer and
digest verification. Volumes use an explicit API transfer/export adapter and
validate size, format, checksum, and attachment state. Keypairs transfer only
public keys. Flavor mapping is an explicit destination capability mapping.
Network, policy, L3, and public-address resources are translated into canonical
O3K resources. Servers are created only after dependency validation and cold
quiescence.

Preflight rejects missing capability, unsupported encryption/format, mutable
source state, scope mismatch, duplicate mapping, insufficient destination
capacity, or unapproved translation with a stable, non-enumerating reason.

## 5. Cutover and rollback

Cutover requires a complete manifest, validated nodes, verified digests, durable
destination ownership, no unresolved `unknown-outcome`, source quiescence
evidence, and a fresh authorization decision. The marker is committed before
success is reported. Source resources are not deleted by v1. Pre-cutover
compensation deletes only migration-owned resources in reverse dependency
order. Post-cutover repair is forward reconciliation under O3K authority.

## 6. Threat model

The contract protects against credential leakage, manifest tampering, replay,
cross-tenant confusion, malicious metadata/image content, SSRF, duplicate
creates after timeout, unauthorized cutover, and audit disclosure. Endpoints
are configured allowlists; TLS/certificate policy, transfer sizes, timeouts,
retries, and concurrency are bounded. Logs contain IDs, digests, phases, and
correlation, not secrets or raw payloads. Failure is closed when identity,
scope, integrity, or authority cannot be established.

## 7. Evidence and exclusions

Gates are contract/domain tests, portable simulated two-cloud tests,
process-level public API tests, bounded real-cloud evidence, restart/failure
matrix, and final OpenTofu NO-OP. The real gate proves zero cross-scope reads or
writes, zero foreign-state changes, no credential leaks, exact cleanup, resume,
rollback, cutover, and canonical integrity. This profile does not claim generic
OpenStack compatibility, live migration, active-active, multi-region,
automatic source deletion, password/token/private-key migration, broad UUID
preservation, or production HA/SLA.
