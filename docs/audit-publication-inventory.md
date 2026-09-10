# B0 Audit publication inventory

This inventory covers production Cloud Kernel mutation and authorization paths.
Mandatory control-plane events use the awaited `record_required_async` boundary;
read-only compatibility telemetry is not promoted to mandatory evidence.

| Service/family | Operations and phases | Classification | Publication boundary |
|---|---|---|---|
| Compute lifecycle | create, delete, provider outcome, authorization denial | MANDATORY_DURABLE | `ComputeService::record_required_audit` |
| Compute domain actions | start, stop, reboot, authorization denial, outcome | MANDATORY_DURABLE | `ComputeService::record_required_audit` |
| Compute keypairs | create/delete and authorization denial | MANDATORY_DURABLE | `ComputeService::record_required_audit` |
| Compute flavors | administrative create/delete and authorization denial | MANDATORY_DURABLE | `ComputeService::record_required_audit` |
| Compute attachments | attach/detach and authorization denial | MANDATORY_DURABLE | `ComputeService::record_required_audit` |
| Native generic mutation | volume/create/update and authorization denial | MANDATORY_DURABLE | `DurableAuditSink::record_required_async` |
| Image | create/upload/delete/artifact resolution and authorization denial | MANDATORY_DURABLE | `ImageService::record_required_audit` |
| Network canonical | network/realm/pool/endpoint mutations and authorization denial | MANDATORY_DURABLE | `NetworkService::record_required_audit` |
| Network resources | network/subnet/port/router/security-group/floating-IP mutations and authorization denial | MANDATORY_DURABLE | `NetworkService::record_required_audit` |
| Compatibility mutations | OpenStack network/image mutations sharing canonical authority | MANDATORY_DURABLE | service required-publication helper |
| Read-only projections | successful list/show/read operations | OPTIONAL | no mandatory publication |
| Unit/integration tests | explicitly injected `MemoryAuditSink`/`FnAuditSink` | TEST_ONLY | test dependency |

The source-level gate `scripts/verify-b0-audit-sink.sh` rejects legacy
`audit_sink.record(...)` calls in mandatory production modules.  New mandatory
sites must be added to this inventory and use the required async helper.
