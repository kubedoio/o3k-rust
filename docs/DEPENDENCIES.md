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
| tracing, tracing-subscriber | O3K maintainers | structured diagnostics | crates.io; MIT/Apache-2.0 family |
| url | O3K maintainers | SQLx URL/IDNA compatibility | crates.io; MIT/Apache-2.0 family; pinned to patched 2.5.7 |

The policy also permits `CDLA-Permissive-2.0` for the transitive
`webpki-roots` dependency. This is an intentional, reviewed exception to the
common license set; BSL and LGPL alternatives remain disallowed unless a
future dependency review explicitly changes `deny.toml`.

The only current advisory exception is RustSec RUSTSEC-2025-0134 for the
unmaintained `rustls-pemfile` 2.2.0 pulled by tonic 0.12.3. It is not a
vulnerability, has no safe upgrade in that dependency line, and is isolated
to the provider transport's certificate handling. The exception is reviewed
on every tonic upgrade and must not be used for vulnerability advisories.

Local `o3k-*` crates are owned by this repository and use the workspace
Apache-2.0 license. A new direct dependency must add an owner and justification
here in the same change, and must not introduce a git source without an
explicit policy change.
