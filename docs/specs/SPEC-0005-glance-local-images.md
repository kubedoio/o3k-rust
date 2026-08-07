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

Image metadata and blob content have explicit different authorities. Image
metadata (ID and project ownership, name, visibility, status, disk/container
format, size, checksum) is durable-store authority behind the narrow
`ImageRepository` port on the SQLite adapter; restart reconstructs public
image metadata from the store, and directory or sidecar-file naming is never
public identity. Image bytes are filesystem-artifact authority: content paths
are derived only from UUIDs under `images/content`, and names and request
fields never become filesystem paths. Upload content is written to a
temporary file and renamed before the store's conditional queued-to-active
activation, which seals size and SHA-256 checksum. Authenticated
`GET /v2/images/{id}/file` revalidates ownership, size, and SHA-256 before
returning bytes, and active metadata with a missing or corrupt artifact fails
closed instead of inventing or reactivating bytes. The adapter uses a bounded
upload limit and does not expose staging or filesystem errors publicly.

Protected OpenStack CLI evidence remains a follow-up for the durable
resource/API workflow issues. Portable API evidence is recorded in the
compatibility inventory; protected-runner evidence is not fabricated.
