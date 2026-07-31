# ADR-0099 — Make human review evidence explicit and fail closed

## Status

Accepted for the repository-side preparation of issue #92. This ADR does not
accept the release or close the issue.

## Context

The release plan requires a non-LLM architecture/security review, but the
repository had no common package for recording reviewer identity, reviewed
commit, findings, dispositions, or explicit approval of destructive cleanup.
Without a schema, a prose note or automated test could be mistaken for the
required human decision.

## Decision

Define a versioned `human-architecture-security-review` JSON artifact and a
threat-model checklist covering the privileged and security-critical surfaces
listed by issue #92. Validate it with
`packaging/validate-human-review.sh`. Validation is fail-closed for missing
independent-review identity, scope, findings/dispositions, commit binding,
review record URL, unresolved-risk disclosure, and explicit approvals. A
release invocation must additionally pass `--require-approved`.

The repository does not generate or commit a `human-review.json` artifact.
The validator checks declarations and structure, not the truth of a person's
identity or the quality of their judgment. Those remain human governance
responsibilities.

## Alternatives rejected

- Treating CI, an LLM review, or a signed commit as human approval would
  violate issue #92's closure rule.
- Making the current release gate synthesize an approval artifact would
  fabricate evidence.
- Leaving the format entirely as prose would make required fields and
  release-blocking omissions easy to miss.

## Verification

`tests/human-review-package.sh` covers valid pending preparation, approved
review, missing independent identity, missing finding disposition, and the
required-approved failure path.
