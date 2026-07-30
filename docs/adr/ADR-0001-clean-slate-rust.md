# ADR-0001 — Clean-slate Rust implementation

Status: Accepted

## Decision

O3K Rust is implemented from scratch in Rust using public specifications, Kubedo-authored requirements, and independently created tests. It is not a source translation of another O3K codebase.

## Consequences

- separate repository and history;
- Rust-native domain and provider architecture;
- compatibility established through contracts and tests;
- clean implementation/provenance policy is mandatory;
- initial delivery is slower than mechanical translation but produces clearer ownership and design freedom.
