# P11 implementation prompt — supersession hold

Status: **Paused pending architecture review**

The detailed v1 implementation prompt that previously lived here targeted the
accepted ADR-0170/SPEC-0028 non-overlapping shared-routed-WireGuard profile.
That architecture now has a proposed successor:

- `docs/adr/ADR-0171-addressrealm-encapsulated-edge-fabric.md`
- `docs/specs/SPEC-0029-addressrealm-encapsulated-edge-fabric-v2.md`
- `contracts/p11-realm-overlay-fabric.md`
- `docs/P11_REALM_OVERLAY_IMPLEMENTATION_PROMPT.md`
- issue #705

## Do not continue privileged P11 fabric implementation from this file

ADR-0170/SPEC-0028 remain historical accepted authority until the successor is
explicitly accepted, but the project is intentionally reconsidering the fabric
before further privileged implementation because the v1 shared IP-only fabric
cannot safely support overlapping customer CIDRs.

PR #703 has already merged a portable semantic endpoint-directory/planning
slice. That work should be preserved where compatible, but no new privileged
Geneve/WireGuard/realm-fabric work should proceed until ADR-0171/SPEC-0029 are
reviewed and accepted or rejected.

If ADR-0171/SPEC-0029 are accepted, use
`docs/P11_REALM_OVERLAY_IMPLEMENTATION_PROMPT.md` as the implementation prompt
and follow the repository's explicit supersession records.

If ADR-0171/SPEC-0029 are rejected, restore/update a v1 implementation prompt
from the still-accepted ADR-0170/SPEC-0028 authority rather than guessing from
git history.

Architecture text and prompt files do not create a product/support claim.
