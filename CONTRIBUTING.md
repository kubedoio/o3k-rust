# Contributing to O3K Rust

O3K Rust is issue-driven and specification-first. Human contributors and LLM agents follow the same acceptance rules.

## Before starting

1. Choose or create one GitHub issue.
2. Read `AGENTS.md` and the linked ADR/spec documents.
3. Confirm the issue has explicit acceptance criteria, tests, non-goals, and provenance restrictions.
4. Discuss public contract or architecture changes before implementation.

## Pull requests

- Keep one logical issue per PR.
- Add tests before or with implementation.
- Update contracts, specs, and compatibility evidence when behavior changes.
- Record AI tools and material task instructions in the PR template.
- Link public compatibility sources.
- Do not include non-public code, internal documents, credentials, production data, or generated files without committed source contracts.

## Required checks

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

## Commit and review expectations

Commits should be small, descriptive, and reviewable. Architecture, security, licensing, cryptography, authentication, authorization, provider-destructive operations, protobuf breaking changes, and `unsafe` code require explicit human maintainer review.

## Licensing

Unless explicitly marked otherwise, contributions intentionally submitted for inclusion are accepted under Apache-2.0 as described by section 5 of the license. Contributors must have the right to submit their work.
