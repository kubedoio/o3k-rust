# P15 — Scale and Composition Foundation

This prompt set is the implementation contract for issue #929 (umbrella). Execute
only in order, from fresh protected `origin/main`, after the preceding slice is
merged and verified. P15.0 records the post-Araf re-baseline and creates the
architecture and contract boundary; later prompts must not broaden it.

Profile: P15 spans deployment profiles (TestLab/portable, production-oriented,
small edge). The per-phase deployment/evidence profile is recorded in each
phase prompt; evidence from one profile is not evidence for another.

Authority: ADR-0182 (edge-to-datacenter building-block Cloud OS) is advanced by
ADR-0184 and SPEC-0047 (P15 scale and composition foundation). ADR-0184 is
Proposed until a human review accepts it; implementation under it proceeds as
Proposed-architecture work and no prompt may self-approve.

Order: P15.0 post-Araf re-baseline; P15.1 topology/failure domains and P15.2
service-registry convergence (these two may proceed in either order); P15.3
hierarchical placement; P15.4 CloudProfile composition; P15.5 Building Block
lifecycle; P15.6 init/join bootstrap; P15.7 scale/composition real evidence.

## Execution prerequisites

Do not execute any phase prompt until:

1. P15.0 (#930) is merged and the human approval state of ADR-0184 is recorded
   in the umbrella issue #929 and in the phase PR;
2. each phase verifies its own dependency issues are merged/closed: P15.1
   (#931) and P15.2 (#932) are mutually independent (either order after
   P15.0); P15.3 (#933) requires P15.1 (#931); P15.4 (#934) requires P15.2
   (#932); P15.5 (#935) requires P15.1 (#931), P15.3 (#933), and P15.4 (#934);
   P15.6 (#936) requires P15.4 (#934) and P15.5 (#935); P15.7 (#937) requires
   all of P15.1-P15.6 (#931-#936);
3. the implementation agent fetches the then-current protected `origin/main`
   (baseline 21fe687c387a04f107b6e87fac04060b1c28e449, PR #928, or a later
   protected merge) and never starts from stale branches or worktrees.

Phase order is strict except that P15.1 (#931) and P15.2 (#932) may proceed in
either order relative to each other.

## Historical note

P15.0 (#930) was executed as documentation/planning only. It is complete when
its acceptance criteria in issue #930 are met; it deliberately lands no runtime
behavior. See `P15_0_POST_ARAF_REBASELINE.md`.

## Permanent rules

- The Cloud Kernel owns canonical topology, service/manifest authority,
  placement, composition, and building-block identity; OpenStack projections
  are derived, never authoritative.
- No second topology authority, no second capacity database, no second
  scheduler, no duplicate agent inventory, no parallel bootstrap truth.
- Readiness is not composition: registry existence/Ready,
  CloudProfile desired composition, and catalog consumability are three
  distinct semantics and must not be collapsed.
- External-hosted services retain their own lifecycle (SPEC-0023 boundary);
  O3K profiles select/expect them, never absorb them.
- Every phase records the Required agent plan fields as a pre-flight
  checklist and honors the validation ladder in AGENTS.md.
- Never weaken Cloud Kernel invariants, never add unprofiled compatibility,
  never hide a missing evidence gate in the runner, never upgrade a claim
  beyond its evidence tier.
