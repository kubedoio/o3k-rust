# ADR-0133 — Exercise every Cargo feature in normal CI

Status: Accepted for the repository-side implementation of issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, cli, governance

#78. This does not
claim agent-backed libvirt lifecycle or real-host execution.

## Decision

The required Rust CI test command is `cargo test --workspace --all-features`,
matching the repository contributor contract and the all-features clippy job.
CI installs the `libvirt-dev` and `pkg-config` native dependencies needed to
link the libvirt feature on the hosted Ubuntu runner. It then cleans the
cached `virt-sys` package so a cache populated without libvirt cannot preserve
a missing native link directive.
The workflow contract test asserts that the feature-complete command is
present and the default-only workspace command is absent.

## Rationale

Running only default-feature tests can leave libvirt, TLS, or protocol feature
paths uncompiled and untested. All-features compilation is a repository-side
signal that those feature combinations remain buildable; it is not a substitute
for agent-backed or host-gated acceptance.

## Verification

Normal CI runs the command after portable workflow and packaging tests, and
`tests/ci-workflow.sh` protects the command from regressing to a default-only
test.
