# P14 — Independent Review, Merge, and Closure Procedure

For each slice: fetch protected `origin/main`; verify the preceding issue is
merged/closed; read #855, the current slice, this prompt set, current normative
sources, and the exact diff; implement only the slice; run required and
profile-specific gates; create a focused PR; review the actual PR HEAD
independently for authority, IAM, isolation, persistence, recovery, transfer,
compatibility, threat model, contracts, tests, and claims.

Classify every finding as BLOCKER, HIGH, MEDIUM, LOW, or NIT. Fix all actionable
findings on the same PR, rerun validation, and re-review the new exact HEAD
until zero findings remain. Merge only the exact reviewed HEAD through normal
protected rules. Then fetch protected main, verify the merge, and close the
slice issue only after acceptance is genuine.

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
Explicit additional approval required: NO
```

Never weaken P9-P13 or P12-IAM, add unprofiled compatibility, or hide a
missing evidence gate in the runner.
