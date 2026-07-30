# SPEC-0005 — Glance v2 local image subset

Status: Implemented subset

## Supported operations

The initial profile supports project-scoped `POST /v2/images`,
`PUT /v2/images/{id}/file`, list, show, and delete. Metadata starts `queued`
and becomes `active` only after bounded content upload, SHA-256 checksum, size
recording, and atomic publication.

Active content is immutable. A second upload returns conflict. Image ownership
comes only from the verified `X-Auth-Token` project claim; client fields cannot
select another project. Inaccessible images are concealed as not found.

## Storage decision

Image metadata is persisted in an atomically replaced `metadata.json` file under
the configured data directory. Content paths are derived only from UUIDs under
`images/content`; names and request fields never become filesystem paths. Upload
content is written to a temporary file and renamed before metadata is marked
active. The adapter uses a 16 MiB default limit and does not expose staging or
filesystem errors publicly.

SQLite image metadata integration and process-level OpenStack CLI evidence remain
follow-ups for the durable resource/API workflow issues. The compatibility
matrix therefore marks the Glance rows `implemented`, not `verified`.
