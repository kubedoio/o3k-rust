# P15 — Independent Review, Merge, and Closure Procedure

For each slice: fetch protected `origin/main` (baseline
21fe687c387a04f107b6e87fac04060b1c28e449, PR #928, or a later protected
merge); verify the preceding issues are merged/closed per the dependency order
in `README.md` (P15.1/P15.2 may proceed in either order); read #929, the
current slice issue, this prompt set, current normative sources, and the exact
diff; implement only the slice; run required and profile-specific gates; create
a focused PR; review the actual PR HEAD independently for authority, IAM,
isolation, persistence, recovery, compatibility, threat model, contracts,
tests, and claims.

Classify every finding as BLOCKER, HIGH, MEDIUM, LOW, or NIT. Fix all
actionable findings on the same PR, rerun validation, and re-review the new
exact HEAD until zero findings remain. Merge only the exact reviewed HEAD
through normal protected rules. Then fetch protected main, verify the merge,
and close the slice issue only after acceptance is genuine.

Reminder: ADR-0184/SPEC-0047 are Accepted (human architecture approval granted
2026-09-12, recorded on PR #938). Every phase PR must record the per-phase
authorization line for its current state — P15.1: `P15.1 implementation
authorized: YES` (authorized after P15.0 #930 is merged); P15.2–P15.7:
authorized under the accepted architecture once their listed dependencies are
merged and review passes. No agent may self-approve beyond this recorded
decision, and the review loop must confirm the authorization line matches the
human's recorded decision.

Merge gate:

```text
BLOCKER: 0
HIGH: 0
MEDIUM: 0
LOW: 0
NIT: 0
Required CI/evidence: PASS
Exact PR HEAD reviewed: YES
Merge authorized by review loop: YES
Explicit additional approval required: NO — beyond the ADR-0184 authorization line recorded per phase
```

Closing rules — never weaken the Cloud Kernel invariants, never add unprofiled
compatibility, never hide a missing evidence gate in the runner, and never
upgrade a claim beyond its evidence tier.
