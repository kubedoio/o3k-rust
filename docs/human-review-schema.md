# Human review evidence schema

Issue #92 requires an independent human review. This schema records the
reviewer's identity, scope, findings, and explicit approvals without treating
automated tests or an LLM review as human approval.

The artifact is a JSON object with `schema_version: 1` and
`artifact_type: "human-architecture-security-review"`:

| Field | Requirement |
|---|---|
| `status` | `pending`, `approved`, or `rejected`; release decisions require `approved` |
| `reviewer` | non-empty `name`, `organization`, `role`, and `is_implementing_agent: false` |
| `reviewed_commit` | exact 40-character lowercase commit SHA |
| `review_record_url` | durable `https://` link to the human review record |
| `scope` | non-empty list of reviewed surfaces |
| `findings` | list of objects with unique, non-empty `id`, severity, and disposition; severity is `critical`, `high`, `medium`, `low`, or `informational`; disposition is `fixed`, `accepted`, `deferred`, or `not-applicable` |
| `approvals` | `release_blocking_findings: true` and `destructive_cleanup: true` only when explicitly approved by the reviewer |
| `unresolved_risks` | list of strings, including an empty list when none remain |

Validate a prepared artifact with:

```text
packaging/validate-human-review.sh --input human-review.json
packaging/validate-human-review.sh --input human-review.json --require-approved
```

The validator is deliberately fail-closed for missing identity, missing
findings/dispositions, missing approvals, malformed commit evidence, and a
non-approved release decision. It does not verify that a person actually
performed the review; that remains a human governance responsibility.
