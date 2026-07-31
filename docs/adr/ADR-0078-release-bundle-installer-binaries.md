# ADR-0078 — Install prebuilt binaries from release bundles

## Status

Accepted

## Context

`packaging/make-release.sh` creates a self-contained bundle with executable
files under `bin/` and the installer under `packaging/`. The installer
previously assumed that it was being run from a source checkout and always
invoked Cargo when `--binary` was omitted. A release bundle has no
`Cargo.toml`, so installation failed before it could copy its included
`o3kd` binary (and, for the libvirt profile, `o3k-compute`).

## Decision

When an explicit `--binary` or `--compute-binary` is not supplied, the
installer first selects the corresponding executable from `ROOT_DIR/bin/`.
If the bundled executable is absent, it retains the existing source-tree
fallback and builds from `ROOT_DIR/Cargo.toml`. Explicit binary paths always
remain authoritative and are still validated as executable files.

The bundle test uses a temporary bundle-shaped directory without a Cargo
manifest and a failing Cargo shim. It verifies that the bundled executable is
installed and that Cargo is not required.

## Consequences

Release bundles are self-contained for the fake profile and the same selection
rule applies to the libvirt compute binary. Source-checkout installation and
explicit binary overrides remain compatible. The test does not claim that a
libvirt host is available; libvirt preflight and host acceptance remain
separate release requirements.
