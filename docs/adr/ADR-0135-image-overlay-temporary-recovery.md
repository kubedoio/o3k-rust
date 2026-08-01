# ADR-0135 — Recover stale image-overlay temporaries on restart

Status: accepted

`ImageCache` startup removes regular files whose names contain the managed
temporary marker from both the cache root and its managed `overlays` directory.
The check uses non-following metadata, so symlinks and directories are never
followed or removed as if they were cache files.

This closes the crash-recovery gap for `qemu-img` overlay publication: a
process dying after temporary creation no longer leaves stale overlay files
that can accumulate or confuse later inspection. Published overlays and
unrelated files are preserved.

The cleanup remains limited to the cache's own directories. It does not claim
remote image transfer, agent-backed realization, or host acceptance.
