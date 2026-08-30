# Governance and branch protection requirements

## Current state (2026-08-30)

| Control | Status |
|---------|--------|
| `main` branch protection | **Active** — `main-protection` repository ruleset |
| Repository rulesets | **Active** |
| Required PRs | **Active** — one approval, last-push approval, thread resolution |
| Required CI checks | **Active** — `rust`, `supply-chain`, `installer-negative`, Tempest Gate A, Mock Cinder |
| Force push protection | **Active** |
| Deletion protection | **Active** |
| Administrator bypass | **Disabled** — bypass is not available |

## Protected merge requirements

The active `main-protection` ruleset applies these requirements to the default
branch. Verify the live ruleset before a protected merge; this document is a
record of policy, not proof that GitHub is configured.

### Rule: `main`

1. **Require a pull request before merging**
   - Require approvals: 1
   - Dismiss stale reviews: yes
   - Require review from code owners: yes

2. **Require status checks**
   - Require branches to be up to date: yes
   - Required checks:
     - `ci / rust` (the full CI workflow including fmt, clippy, check, test)
     - `maintainability-guards` (the R0 guardrail check)

3. **Require conversation resolution before merging**: yes

4. **Enforce for administrators**: yes

5. **Lock branch**
   - Allow force pushes: no
   - Allow deletions: no

### Verification

Inspect the live ruleset before each protected merge:

```bash
gh api repos/kubedoio/o3k-rust/rulesets --jq ".[] | select(.name == \"main-protection\")"
```

The current required-check names are the five listed in the state table above.
The permanent architecture guard runs inside the `rust` check; it is not a
separate historical `maintainability-guards` status context.
