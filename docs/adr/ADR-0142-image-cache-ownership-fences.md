# ADR-0142 — Fence image cache directories and temporary artifacts

Status: accepted

The image cache rejects symlinked or non-directory cache roots, `base`, and
`overlays` children before creating or publishing artifacts. Existing overlays
are revalidated with `qemu-img` against their expected managed backing image
before they are reused.

Startup cleanup removes only generated base, overlay, and image-upload
temporary names whose UUID/checksum components match the service patterns;
unrelated temporary-looking files are preserved. Overlay deletion also fails
closed for non-regular targets.

This is the repository-side safety boundary for issue #79. Authenticated
Glance-to-agent transfer, real qemu-img host evidence, and full guest lifecycle
remain separately gated.
