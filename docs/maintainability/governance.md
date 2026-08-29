# Governance and branch protection requirements

## Current state (2026-08-28)

| Control | Status |
|---------|--------|
| `main` branch protection | **Absent** — anyone with push access can push directly |
| Repository rulesets | **Absent** |
| Required PRs | **Absent** |
| Required CI checks | **Absent** — CI runs on PR but is not a required gate |
| Force push protection | **Absent** |
| Deletion protection | **Absent** |
| Administrator bypass | N/A — no protection exists to bypass |

## Required settings before refactor merges

Before any #758 structural PR is merged to `main`, apply these branch
protection rules using the GitHub UI (Settings > Branches > Add rule) or
the `gh api` command below.

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

### Application command

```bash
gh api repos/kubedoio/o3k-rust/branches/main/protection \
  --method PUT \
  --input - <<'EOF'
{
  "required_status_checks": {
    "strict": true,
    "contexts": [
      "ci / rust",
      "maintainability-guards"
    ]
  },
  "enforce_admins": true,
  "required_pull_request_reviews": {
    "required_approving_review_count": 1,
    "dismiss_stale_reviews": true,
    "require_code_owner_reviews": true
  },
  "restrictions": null,
  "allow_force_pushes": false,
  "allow_deletions": false,
  "lock_branch": false
}
EOF
```

> **Note**: The `maintainability-guards` check must be registered in CI first
> (see CI workflow), otherwise GitHub will not offer it as an option in the
> status checks list. Apply the branch protection after at least one successful
> CI run that includes the guard step.
