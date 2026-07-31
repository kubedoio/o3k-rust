# ADR-0054: Fail closed on ambiguous libvirt ownership discovery

Status: Accepted

## Context

O3K ownership is established from signed-by-construction metadata in domain
XML, not from the human-readable domain name. If two domains advertise the
same O3K server ID, selecting the first result could let reconciliation mutate
an ambiguous or foreign resource. Domain source values also reach libvirt XML
and must not carry control characters, traversal components, or arbitrary URI
schemes.

## Decision

`discover_domain_xmls` counts owned metadata by server ID and quarantines every
owned result for an ID that occurs more than once. No duplicate is eligible
for provider mutation. Domain XML construction rejects empty/control-character
sources, URI-like values, and parent-directory components before escaping the
value into the XML attribute.

## Consequences

Ambiguous ownership becomes observable and requires operator/reconciliation
resolution rather than an unsafe best-effort mutation. The source validation is
an input boundary, not a substitute for verified image artifact resolution;
image preparation and host-backed evidence remain separate concerns.
