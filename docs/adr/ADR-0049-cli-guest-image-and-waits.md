# ADR-0049 — Upload the guest image and wait for CLI lifecycle evidence

## Status

Accepted

## Context

The libvirt CLI workflow created image metadata without uploading a guest
image, started server operations without waiting for terminal transitions, and
accepted an empty console response. Those steps could report a successful
command sequence without proving a bootable guest or usable console.

## Decision

Require `O3K_TESTLAB_IMAGE_PATH` to name a local guest image, pass it through
the public `openstack image create --file` operation, use `--wait` for server
create/stop/start/reboot, and poll console output for a bounded number of
attempts. An empty console after the polling budget is a lifecycle failure;
missing input or unavailable credentials/endpoint remains an explicit skip.

## Consequences

The workflow now exercises image data transfer and records stronger lifecycle
evidence without direct database or libvirt access. The image path is local
test input and is never written to the redacted artifact. Guest boot semantics
still depend on the configured real-libvirt deployment and image fixture.
