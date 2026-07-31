# ADR-0055 — Read-only libvirt config-drive attachment

## Status

Accepted for the libvirt XML boundary.

## Context

The config-drive subsystem publishes an owned, deterministic OpenStack layout,
but the domain builder previously emitted only the guest boot disk. A guest
could therefore never consume the generated metadata through libvirt.

## Decision

`DomainSpec` accepts an optional host-side path to a materialized config-drive
image. When present, the builder validates it with the same fail-closed source
rules as the boot image and emits a raw, read-only SATA CD-ROM attachment. The
path is XML-escaped and is not accepted as a URI, control-character value, or
parent-directory path.

This change only wires a materialized artifact into domain XML. It does not
turn the generated directory into an image, verify O3K ownership at this
boundary, claim that a guest has booted or consumed cloud-init, or claim that a
Nova-facing request currently supplies the path; those remain integration and
host evidence work.

## Consequences

Generated domains can expose a materialized config-drive without permitting the
guest to write it. Invalid or ambiguous host sources fail before definition,
and the existing deterministic XML behavior remains unchanged when no
config-drive is requested.
