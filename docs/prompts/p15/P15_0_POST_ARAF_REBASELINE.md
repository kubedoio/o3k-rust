# P15.0 — Post-Araf Re-Baseline (Historical Record)

Issue: #930. This file is the historical record of the P15.0 re-baseline
itself; it is not an implementation prompt. P15.0 was executed as
documentation/planning only and deliberately landed no runtime behavior.

Baseline for the whole P15 program: protected `origin/main`
21fe687c387a04f107b6e87fac04060b1c28e449 (PR #928, Araf P2 convergence).
Every later phase fetches the then-current protected `origin/main` and must not
start from stale branches or worktrees.

## What P15.0 delivered

The post-Araf current-state audit and the edge-to-datacenter gap register were
produced against the PR #928 baseline, and the P15 architecture was written
down before any implementation was authorized. Deliverables:

- Audit doc: `docs/architecture/p15-0-post-araf-current-state-audit.md`.
- Gap register: `docs/architecture/p15-e2d-gap-register.md`.
- ADR-0184: `docs/adr/ADR-0184-p15-scale-and-composition-foundation.md`
  (status Proposed until human review accepts it).
- SPEC-0047: `docs/specs/SPEC-0047-p15-scale-and-composition-foundation.md`
  (status proposed).
- This prompt set: `docs/prompts/p15/` (README, P15.0 record, P15.1-P15.7,
  REVIEW_AND_MERGE).
- README/roadmap reconciliation recording the P15 program and its order.
- Plan for issue #433 as input to the re-baseline.
- Program issues: umbrella #929; P15.0 #930; P15.1 #931; P15.2 #932; P15.3
  #933; P15.4 #934; P15.5 #935; P15.6 #936; P15.7 #937.

## Authorization state

ADR-0184 is Proposed. Until a human review accepts it, implementation under it
proceeds as Proposed-architecture work, and every phase PR must record
`P15.N implementation authorized: NO — awaiting human architecture approval`
when the human has not approved. No agent may self-approve.

## Completion

```text
P15.0 audit doc present: YES
P15.0 gap register present: YES
ADR-0184 present (Proposed): YES
SPEC-0047 present (proposed): YES
Prompt set P15.1-P15.7 + README + REVIEW_AND_MERGE present: YES
README/roadmap reconciliation: YES
#433 plan recorded: YES
Issues #929-937 filed: YES
Runtime behavior landed: NONE
P15.0 acceptance criteria in #930 met: YES
```
