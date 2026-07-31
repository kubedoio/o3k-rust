# Architecture and security review package

This checklist is the repository-side input to issue #92. It makes the scope
and evidence expected from an independent reviewer explicit; it is not a
review and cannot satisfy the issue's human approval requirement.

## Review identity and decision

- [ ] Reviewer is a named human maintainer or security reviewer, with
      organization and role recorded.
- [ ] Reviewer is not the implementing LLM agent.
- [ ] The exact reviewed commit and a durable review-record URL are recorded.
- [ ] Every finding has an identifier, severity, and disposition.
- [ ] Release-blocking findings are fixed or explicitly accepted by a
      responsible human maintainer.
- [ ] Destructive cleanup and foreign-state protections receive explicit
      approval.
- [ ] Unresolved risks are recorded rather than implied to be absent.

## Threat-model prompts

For each surface, record the actor, asset, trust boundary, abuse case,
fail-closed behavior, and test/evidence reference.

| Surface | Minimum review prompts |
|---|---|
| Keystone and project isolation | Can a token cross projects, roles, or tenants? Are invalid/expired credentials rejected without resource disclosure? |
| Compute-agent mTLS | Are identity, authorization, rotation, replay, and stream binding enforced before privileged commands? |
| Journal and reconciliation | Are intent, idempotency, timeout/unknown outcomes, restart, and stale observations safe? |
| Placement and scheduler | Can duplicate names, partial publication, or failed allocation leak capacity or bypass policy? |
| Images and paths | Are image credentials, symlinks, backing chains, path traversal, and partial cleanup bounded? |
| Config-drive | Are user-data, metadata, and credentials size-bounded, scoped, and removed on failed publication? |
| Libvirt and ownership | Can XML, domain state, or discovery act on a foreign or ambiguous domain? |
| Bridge/TAP/DHCP | Can an existing interface, TAP, dnsmasq process, or port be overwritten or deleted when foreign? |
| Console and logs | Are reads authorized, bounded, serialized, and redacted? |
| Installer/reset/uninstall/runner | Are service, filesystem, host, and runner mutations restricted to owned targets and trusted workflows? |

## Evidence contract

The machine-readable package is described in
[`human-review-schema.md`](human-review-schema.md) and checked by
`packaging/validate-human-review.sh`. A pending artifact is useful for
tracking preparation. Only an artifact that passes with
`--require-approved`, contains an independent human identity, and links the
review record can support the release decision.

No `human-review.json` is committed by this change because doing so would
fabricate reviewer identity or approval.
