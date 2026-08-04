# ADR-0116 — Bind libvirt config-drive attachment to verified bytes

Status: Accepted for the issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, governance

#80 repository attachment slice.

## Context

The config-drive publisher owns a deterministic directory and verifies its
manifest, but a libvirt domain definition receives a host path. A path alone
could be replaced or altered between orchestration and XML generation, making
the attached artifact different from the one the caller intended.

## Decision

Represent an optional config-drive attachment as a host path plus a SHA-256
digest. Before producing XML, require an absolute regular file and recompute
the digest from its bytes. Only an exact match is attached as a read-only raw
SATA CD-ROM; invalid, symlinked, altered, or ambiguous paths fail closed.

## Evidence boundary and non-goals

This is a portable XML-boundary safety contract. It does not generate ISO/VFAT
media, connect Nova or the compute agent to the path, prove guest cloud-init
consumption, or provide real-host evidence.

## Consequences

Callers must carry the digest alongside a materialized artifact. The
config-drive store can hand off bounded verified ISO bytes plus `iso`, size,
and SHA-256 metadata without exposing its host path. The domain builder cannot
silently attach stale or modified bytes, while the existing read-only
attachment and XML escaping behavior remain unchanged.
