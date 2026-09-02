# P12-IAM — Independent Review, Merge and Closure Procedure

Use this procedure for every P12-IAM implementation slice.

## Execution prerequisite

Do not begin P12-IAM implementation until P13 #744 and P13.7 #752 are closed with accepted evidence.

## Per-slice workflow

1. Fetch current protected `origin/main`.
2. Confirm previous P12-IAM slice is merged.
3. Read the umbrella issue, slice issue, this prompt set, current normative IAM sources and current code.
4. Create a fresh branch from current `origin/main`.
5. Implement only the slice scope.
6. Run all required gates.
7. Inspect the complete `origin/main...HEAD` diff.
8. Perform an independent review of authority, security, persistence, compatibility and test evidence.
9. Push and open a focused PR linked to the slice issue.
10. If HEAD changes after review, re-review the new exact HEAD.
11. Merge only through protected repository rules with expected-head protection where available.
12. Fetch newly merged `origin/main` before starting the next slice.

Do not stack all P12-IAM slices on one branch.

## Mandatory review questions

For every semantic slice verify:

### Authority

- Is O3K still canonical for PrincipalId, scope, assignment, authorization and native AuthContext?
- Did any IdP display/group claim accidentally become cloud authority?
- Did Araf/browser concerns enter O3K IAM?

### Authentication security

- Are raw external/native tokens bounded, redacted and discarded from ordinary domain state?
- Is issuer/audience/signature/time validation fail-closed?
- Is unknown identity/scope non-enumerating?
- Is system/operator access explicit and least-privileged?

### Isolation

- Can a tenant discover/read/mutate another project's identity/resources/Operations?
- Can a tenant reach system/operator routes?
- Can a service principal broaden the original actor implicitly?

### Persistence/recovery

- Are new identity records durable and migration-safe?
- Is SQLite/PostgreSQL behavior equivalent where supported?
- Does restart preserve canonical identity without depending on process memory?

### Compatibility

- Native password/token flows unchanged unless intentionally/versionedly extended?
- Keystone-compatible behavior preserved?
- Existing P13/OpenStack compatibility gates unaffected?

### Genericity

- No Keycloak-specific canonical model?
- No Araf-specific endpoint/business rule?
- No provider-specific IAM behavior hidden in service code?

## Minimum validation

Run at least the repository-required equivalents of:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo check --workspace --all-targets --all-features
python3 scripts/check-architecture-boundaries.py
python3 scripts/check-maintainability-guards.py
bash tests/maintainability-guards.sh
cargo nextest run --workspace --all-features --profile pr
cargo test --workspace --all-features
```

plus targeted IAM/API/PostgreSQL/real-IdP gates required by the slice.

## Finding policy

Report findings as:

- BLOCKER
- HIGH
- MEDIUM
- LOW/NIT

Do not merge with unresolved BLOCKER/HIGH findings affecting the selected profile.

## Final closure review

After P12-IAM.8 merges, perform a fresh read-only closure review on current protected `main`.

Confirm:

- P13 remains closed/unchanged in authority;
- every P12-IAM issue has evidence;
- real OIDC provider evidence passes;
- tenant/operator isolation passes;
- public contracts match implementation;
- Araf no longer requires fixture identity for the supported production auth path;
- no hidden browser/IAM authority exists in Araf BFF;
- no secrets were committed/uploaded;
- no BLOCKER/HIGH finding remains.

Authorize umbrella closure only with:

```text
P12-IAM aggregate verdict: PASS
P12-IAM umbrella closure authorized: YES
Araf production identity unblocked: YES
```

Otherwise keep the umbrella open with exact blockers.
