# Dependency review record

This is the owner and purpose record for every direct external dependency in
the workspace. `O3K maintainers` owns review of each entry. Versions and the
resolved transitive graph are pinned in `Cargo.lock`; `cargo-deny` checks
licenses, advisories, bans, and sources in CI.

| Dependency | Owner | Purpose | Source/license review |
| --- | --- | --- | --- |
| axum, tower, http | O3K maintainers | HTTP routing and middleware | crates.io; MIT/Apache-2.0 family |
| async-trait, tokio | O3K maintainers | async traits and runtime | crates.io; MIT/Apache-2.0 family |
| serde, serde_json, toml | O3K maintainers | typed configuration and API data | crates.io; MIT/Apache-2.0 family |
| base64, hmac, sha2 | O3K maintainers | token and digest primitives | crates.io; MIT/Apache-2.0 family |
| sqlx | O3K maintainers | SQLite persistence and migrations | crates.io; MIT/Apache-2.0 family |
| thiserror, time, uuid | O3K maintainers | errors, timestamps, resource IDs | crates.io; MIT/Apache-2.0 family |
| prost, tonic, tonic-build | O3K maintainers | provider protobuf/gRPC boundary | crates.io; MIT/Apache-2.0 family |
| virt (optional) | O3K maintainers | local libvirt/KVM FFI adapter | crates.io; LGPL-2.1; enabled only for `o3k-compute --features libvirt` |
| tracing, tracing-subscriber | O3K maintainers | structured diagnostics | crates.io; MIT/Apache-2.0 family |
| url | O3K maintainers | SQLx URL/IDNA compatibility | crates.io; MIT/Apache-2.0 family; pinned to patched 2.5.4 |

The policy also permits `CDLA-Permissive-2.0` for the transitive
`webpki-roots` dependency. This is an intentional, reviewed exception to the
common license set; the optional `virt` binding is the reviewed LGPL-2.1
exception required for the local libvirt adapter. BSL alternatives remain
disallowed unless a future dependency review explicitly changes `deny.toml`. Internal path
dependencies are intentionally allowed; the source gate rejects unknown
external registries and git sources.

The current advisory exceptions are:
- RustSec RUSTSEC-2023-0071 for `rsa` 0.9.10, which is retained only in the
  lockfile through SQLx's optional MySQL support. This workspace enables SQLite
  only; `cargo tree` has no active RSA path. It must be reconsidered if another
  SQLx database feature is enabled.

The remaining ID is passed explicitly to `cargo audit`; all other advisories
fail CI. The exception is reviewed whenever SQLx features change.

Tonic 0.13.1 is intentionally used instead of 0.12.3: its rustls PEM
conversion fixes the mTLS client-identity failure covered by the compute-agent
black-box test. The upgrade also removes the former rustls-pemfile advisory
exception.

Local `o3k-*` crates are owned by this repository and use the workspace
Apache-2.0 license. A new direct dependency must add an owner and justification
here in the same change, and must not introduce a git source without an
explicit policy change.
