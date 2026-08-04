# ADR-0115 — Bridge verified image artifacts into the local cache

Status: Accepted for the issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: network, image, governance

#79 repository integration slice.

## Context

`ImageService::resolve_artifact` verifies project ownership, active metadata,
regular-file content, size, and SHA-256 before returning an image artifact.
`ImageCache::cache_base` independently validates bytes before publishing a
content-addressed base file. Without an explicit adapter, callers had to
manually copy the artifact fields across that boundary and could accidentally
discard or mismatch the verified size metadata.

## Decision

Add `ImageCache::cache_artifact`, which accepts an `ImageArtifact`, verifies
that its declared size matches its bytes, delegates digest/format/limit checks
and atomic publication to `cache_base`, and returns a typed
`CachedImageArtifact` containing the local managed path and immutable identity
metadata. Repeated calls remain idempotent through the existing content-
addressed cache behavior.

The returned path is local to the image-cache process. It must not cross the
OpenStack API or compute-agent protocol boundaries.

## Evidence boundary and non-goals

This proves only a portable service-to-cache repository seam. It does not
implement authenticated transfer to a compute host, agent dispatch, durable
image/overlay ownership references, libvirt realization, or real CirrOS/
`qemu-img` host evidence. Those remain required by issue #79's acceptance
criteria.

## Consequences

Future orchestration can use one typed operation to publish a verified Glance
artifact locally without reimplementing validation. The bridge intentionally
does not add a wire-contract field or claim that a remote compute host has
received the image.
