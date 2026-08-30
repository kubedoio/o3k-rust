# Governance and branch protection requirements

## Current state (2026-08-30)

| Control | Status |
|---------|--------|
| `main` branch protection | **Active** — `main-protection` repository ruleset |
| Repository rulesets | **Active** |
| Required PRs | **Active** — one approval, last-push approval, thread resolution, extra approval for unattributed changes |
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
   - Require review from code owners: no
   - Require approval of the last push: yes
   - Require an extra approval for unattributed changes: yes

2. **Require status checks**
   - Require branches to be up to date: yes (strict required-status freshness)
   - Required checks:
     - `rust`
     - `supply-chain`
     - `installer-negative`
     - `Tempest portable preflight (Gate A)`
     - `Mock Cinder Component Test (not a real Cinder deployment)`

3. **Require conversation resolution before merging**: yes

4. **Bypass actors**: none

5. **Allowed merge methods**: merge, squash, rebase

6. **Lock branch**
   - Allow force pushes: no
   - Allow deletions: no

### Verification

Inspect the live ruleset before each protected merge:

```bash
gh api repos/kubedoio/o3k-rust/rulesets --jq ".[] | select(.name == \"main-protection\")"
```

The current required-check names are the five listed in the state table above.
Strict required-status freshness is enabled. There is no protected merge queue
configured as an alternative freshness mechanism.
The permanent architecture guard runs inside the `rust` check; it is not a
separate historical `maintainability-guards` status context.
