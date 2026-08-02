# ADR-0153 — Static Rust and OpenStack release policy

Status: Accepted
Date: 2026-08-02
Review date: 2027-08-02
Responsible maintainer: O3K maintainers
Supersedes: provisional Rust 1.95/Flamingo-only draft decisions
Superseded-by: none

## Context

The compiler channel and OpenStack release series are compatibility inputs. A
moving Rust `stable` channel or runtime selection of an advertised OpenStack
release changes the product contract without a reviewed specification update.
OpenStack services also use different API versioning mechanisms, so a release
codename alone is not a sufficient wire contract.

## Considered alternatives

1. Continue with Rust 1.85 and discover OpenStack versions at runtime. Rejected:
   it leaves stale compiler behavior and nondeterministic API semantics.
2. Pin Rust 1.95 and target only Flamingo. Rejected for #332: the governed
   modernization requires current Gazpacho primary compatibility while keeping
   Flamingo as a backward profile.
3. Follow the moving stable compiler and expose every discovered microversion.
   Rejected: it cannot provide a reviewable, contiguous compatibility window.

## Decision

O3K Rust builds with exact stable Rust `1.97.1` and uses it as the workspace
MSRV until a later decision changes it. `rust-toolchain.toml` is the toolchain
source of truth; CI and release evidence must consume or verify it.

The primary OpenStack compatibility profile is `2026.1 Gazpacho`. The backward
compatibility profile is `2025.2 Flamingo`. The machine-readable
`compatibility/openstack-targets.yaml` manifest records service-specific API
versions, advertised maxima, O3K's implemented window, headers, extensions,
unsupported operations, client versions, and evidence state.

The label `2026.1 Flamingo` is invalid and must never occur in code, specs,
tests, reports, or release notes. Runtime discovery may preflight a declared
profile, but must not select a different profile or `latest`. Nova's baseline
window is the exact implemented microversion `2.1`; it is not advertised as
support through the profile maxima. Placement allocation writes use the exact
implemented microversion `1.28`.

## Consequences

The target is reproducible and machine-checkable. A newer point release may
satisfy the same series profile, but it does not silently change the declared
wire contract. New language features and dependencies must build with Rust
1.97.1. A service or microversion is not supported merely because it appears in
the OpenStack catalog; it needs portable and protected evidence states.

## Migration and rollback

Update toolchain metadata, add the target manifest, align the TestLab baseline
and compatibility inventory, add drift checks, then run locked compile,
formatting, clippy, tests, contract checks, and native/libvirt validation. To
roll back, revert the focused #332 change and restore the preceding accepted
documents as one reviewed change; never silently lower the MSRV or rename a
release profile in place.

## Evidence and provenance

- Rust 1.97.1 release announcement: <https://blog.rust-lang.org/2026/07/16/Rust-1.97.1/>
- OpenStack release catalog: <https://releases.openstack.org/>
- Gazpacho deliverables: <https://releases.openstack.org/gazpacho/index.html>
- Flamingo deliverables: <https://releases.openstack.org/flamingo/index.html>
- Nova microversion guide: <https://docs.openstack.org/api-guide/compute/microversions.html>
- Placement 2025.2 microversion history: <https://docs.openstack.org/placement/2025.2/placement-api-microversion-history.html>

The decision is enforced by `tests/compatibility-target.sh` and the CI
toolchain, contract, and release-gate jobs. No private source or implementation
was used.
