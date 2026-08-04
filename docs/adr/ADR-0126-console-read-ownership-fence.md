# ADR-0126 — Revalidate domain ownership before opening a console

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, governance

## Context

The compute agent already validates the requested resource ID before issuing a
console command, but the libvirt adapter previously opened a domain console by
name without independently checking the domain's O3K metadata. A caller that
reached the adapter through another path could therefore read a foreign or
misidentified domain sharing a name or prefix.

## Decision

Console reads require the expected O3K server ID. The libvirt backend fetches
the domain XML, accepts only valid O3K ownership metadata with that exact
server ID, and only then opens the stream. Missing, malformed, foreign, or
mismatched metadata is reported as not found. The agent passes the command's
authenticated resource ID into this fence.

## Consequences

Console access is bound to both the domain name and its durable O3K ownership
identity. Existing bounded, nonblocking console behavior is unchanged for an
owned domain; host guest-output evidence remains a separate acceptance gate.
