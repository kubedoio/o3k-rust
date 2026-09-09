# Integrated production-profile gate (#907)

`tests/integrated-production-profile-gate.sh` is the executable, fail-closed
gate for the integrated O3K control-plane profile. It has two deliberately
different outcomes:

* With no opt-in, it runs process/composition, native API, and the portable
  P12.7 native/compatibility convergence tests and reports **portable evidence
  only**.
* With `O3K_INTEGRATED_GATE_REAL=1`, it requires an HTTPS health URL, a ready
  PostgreSQL database, and a pinned provider binary/version. Missing or failed
  prerequisites report **BLOCKED** (exit 2), never PASS.

The portable P12.7 path proves, against the durable SQLite composition, that
native and selected OpenStack surfaces converge on one resource authority,
cross-project access is concealed, request fields cannot select another owner,
opaque cursors reject tampering, and resources survive an HTTP/store restart.
The real path runs the PostgreSQL P12.4 conformance suite and the P12.7
native/compatibility convergence and restart suite against the supplied
`O3K_DATABASE_URL`, then reruns the native closure and native IAM contract
gates. These are necessary deployment checks, not a complete #907 release
claim. The gate does not claim HA, TLS certificate-authority policy, provider
lifecycle success, federated IdP/Araf process success, or production readiness
by itself. Those claims require the protected host workflow and
profile-specific evidence artifacts.

The repository also contains a separate real Keycloak/Araf process probe:
`O3K_P12_7_AFTER_HOOK=tests/p12-iam-8-real-araf-process.sh
bash tests/p12-iam-7-real-idp.sh`. That probe passed in the current workspace,
including federated scope exchange, Araf callback/session/scope/context and
native resource access. It deliberately uses loopback HTTP and a test-profile
runtime, so it is not production HTTPS or real-provider lifecycle evidence and
must not be promoted to the #907 release gate.

Even when every real-path prerequisite and database suite passes, this script
exits with status 2 because it is not the protected 16-criterion release
workflow.  Callers must treat it as a prerequisite/conformance check and must
obtain the final verdict from the protected workflow's integrated evidence;
there is no successful exit status here that can be mistaken for `GO`.

Required real-path inputs are `O3K_DATABASE_URL`,
`O3K_INTEGRATED_GATE_URL` (must be `https://`), `O3K_PROVIDER_BINARY`,
`O3K_PROVIDER_VERSION`, and `O3K_INTEGRATED_GATE_EVIDENCE`. The latter must
be a runner-produced JSON manifest with `schema_version: 1`,
`profile: "production"`, and exactly one `status: "passed"` evidence
reference for each of the 16 #907 criteria:

`federated_auth_context`, `service_resource_schema_location_discovery`,
`bounded_native_resource_crud`, `native_compute_lifecycle`,
`canonical_operations`, `canonical_relationships`,
`authoritative_quota_enforcement`, `iam_governance`, `durable_audit_restart`,
`operator_diagnostics`, `authoritative_metering`, `cross_project_isolation`,
`secret_non_disclosure`, `process_restart_recovery`,
`database_bounded_collections`, and `openstack_convergence`.

Each `evidence` value must be either a non-empty local regular-file artifact
path relative to the manifest (within the manifest directory) or an HTTPS URL
without embedded credentials; HTTP URLs, fragments, whitespace, absolute
paths, traversal escapes and missing artifacts are rejected. The manifest is
bounded to 4 MiB. Relative artifacts are resolved from the manifest
directory, and symlinks in any path component are rejected. The gate
also rejects group/world-writable manifests and local artifacts, preventing
post-validation mutation by another local user while allowing CI-specific
read modes. The gate
validates this manifest before claiming a real-profile run; it does not accept
a partial artifact or infer missing criteria from endpoint health. Duplicate
JSON keys and non-finite JSON numbers are rejected to avoid parser ambiguity,
and each local artifact is bounded to 64 MiB. Artifact presence and transport
checks still do not establish authenticity—the protected workflow must control
artifact production and retention. These are resource/integrity guards, not a
substitute for protected workflow signing or provenance validation.
Secrets are read from the environment and are not written to evidence.
