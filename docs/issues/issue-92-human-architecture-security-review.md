# Issue #92 — Human architecture and security review

## Audit result

Issue #92 remains open. The acceptance criteria require an identified,
independent human reviewer and explicit approval; repository tests and an LLM
agent cannot provide that evidence.

## Repository-side improvement

This change adds a threat-model checklist, a machine-readable review evidence
schema, and a fail-closed validator for future `human-review.json` artifacts.
The validator checks identity declarations, reviewed commit binding, scope,
finding severity/disposition, unresolved-risk disclosure, and explicit
release/destructive-cleanup approvals. `--require-approved` is required for a
release decision.

No review artifact is committed, no reviewer is named, and no approval is
claimed. Real-host evidence and independent human review remain external
release prerequisites.

## Decision

See [ADR-0099](../adr/ADR-0099-human-review-evidence-package.md).
