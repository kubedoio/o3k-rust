# ADR-0147 — Verify qcow2 format before cache publication

## Status

Accepted for the repository-side issue #79 image-cache boundary.

## Context

The image service records a declared disk format and verifies size and
checksum, but those checks do not prove that bytes declared as `qcow2` are
actually a qcow2 image. Publishing arbitrary bytes under a `.qcow2` cache name
can make later overlay creation fail unpredictably or pass an invalid artifact
to a host boundary.

## Decision

Before publishing a new qcow2 base, and before reusing an existing cache hit,
`ImageCache` invokes the configured `qemu-img info --output=json` tool and
requires the reported format to be `qcow2`. Verification failures leave new
temporary bytes unpublished; an existing cache entry is retained but not
reused. Raw images retain the existing digest/size validation because raw
format has no equivalent structure claim at this boundary.

## Consequences

The qcow2 cache path now requires an available, trustworthy `qemu-img` binary.
This is a repository safety boundary, not proof of authenticated image
transfer, host realization, or real-host qemu evidence.
