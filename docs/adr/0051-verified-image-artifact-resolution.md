# ADR-0051: Verified project-scoped image artifact resolution

Status: Accepted

## Context

The image service already publishes uploaded content atomically and persists its
metadata, but callers had no single contract for resolving an image into a
verified artifact. Passing a logical image ID directly to a provider can cause
the provider to read the wrong file or use content that was modified after
upload.

## Decision

`ImageService::resolve_artifact` accepts a project and image ID and returns a
verified content snapshot only when the image is active, its disk format is
`raw` or `qcow2`, the content is a regular non-symlink file, its size is within
the configured upload bound, and its size and SHA-256 digest still match the
persisted metadata. No host path crosses the public artifact or agent protobuf
boundary. Project scoping remains mandatory for every lookup.

## Consequences

Future provider and agent work can consume a stable artifact ID, format, size,
digest, and verified content snapshot without reopening a mutable host path.
Resolution hashes the content on each call, which favors correctness over
avoiding a read. QEMU validation, transfer to an agent, and real-host evidence
remain separate follow-up work.
