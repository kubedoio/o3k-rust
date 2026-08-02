# ADR-0070 — Bound config-drive network and vendor data

Status: Accepted

## Context

Config-drive generation bounded user-data and metadata but accepted network
and vendor payloads without a size limit. A caller could therefore request an
unbounded temporary publication and consume local storage.

## Decision

Limit serialized network data and raw vendor data to 64 KiB each, validating
both before any temporary directory is created. Reject oversized input with a
typed error and leave any existing published directory untouched.

## Consequences

All caller-controlled config-drive payload classes share an explicit bounded
publication contract. Larger cloud-init extensions require a separately
designed artifact/reference mechanism rather than bypassing local limits.
