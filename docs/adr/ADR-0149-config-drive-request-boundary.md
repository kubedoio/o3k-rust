# ADR-0149 — Reject unsupported config-drive server requests

## Status

Accepted for the bounded issue #80 API safety slice.

## Context

The server-create JSON type previously ignored unknown fields. A caller could
therefore send Nova's `config_drive: true`, receive a normal create response,
and get a server without the requested metadata media. The lower layers do not
yet provide a complete safe path from that request to a materialized
ISO/VFAT artifact: the compute protocol carries an artifact identity and
digest, while the libvirt boundary requires a verified host-local path.

## Decision

Recognize the optional `config_drive` boolean in the server-create request.
`false` remains a no-op in the existing profile; `true` fails with `400 Bad
Request` and an explicit unsupported-profile error before lifecycle intent is
persisted or a provider is called.

This avoids silently changing the caller's requested semantics while keeping
the public protocol and provider boundaries unchanged.

## Non-goals and follow-up

This decision does not generate ISO/VFAT media, resolve an artifact on a
compute host, attach libvirt media, or claim guest cloud-init/reboot evidence.
Those require a separately specified artifact-delivery and Nova/agent
lifecycle slice under issue #80.

## Provenance

The field name and rejection behavior follow the public Nova server-create
contract; the boundary is an O3K fail-closed design decision. No private source
or implementation was used.
