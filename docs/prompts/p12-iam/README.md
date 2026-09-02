# P12-IAM Prompt Set

This directory is the implementation source for the **P12-IAM — Production IAM, OIDC Federation & Console Identity** convergence round.

## Execution prerequisite

Do not execute these prompts until:

1. P13 umbrella #744 is closed;
2. P13.7 #752 is closed with accepted evidence;
3. the implementation agent fetches the then-current protected `origin/main`.

The planning branch/commit that introduced these prompts is never an implementation baseline.

## Order

1. `P12_IAM_0_ARCHITECTURE.md`
2. `P12_IAM_1_OIDC_VALIDATION.md`
3. `P12_IAM_2_FEDERATED_BINDING.md`
4. `P12_IAM_3_SCOPE_DISCOVERY.md`
5. `P12_IAM_4_TOKEN_EXCHANGE.md`
6. `P12_IAM_5_SYSTEM_OPERATOR.md`
7. `P12_IAM_6_PUBLIC_CONTRACTS.md`
8. `P12_IAM_7_REAL_IDP_EVIDENCE.md`
9. `P12_IAM_8_ARAF_CONVERGENCE.md`
10. `REVIEW_AND_MERGE.md` for every slice and final closure.

## Permanent rules

- O3K IAM remains authoritative.
- Federation authenticates an external subject into an O3K principal; it does not make external claims canonical cloud authorization state.
- Use `(issuer, subject)` as the external identity key. Never implicitly bind by email/display name.
- Validate OIDC access tokens with maintained standards-based libraries; do not hand-roll JWT cryptography.
- O3K does not own browser sessions/cookies/callback UX.
- Araf BFF remains the OIDC confidential browser client.
- Existing password/native-token flows and Keystone compatibility must not regress.
- Cross-project and tenant/operator isolation are release-blocking security properties.
- Every semantic slice starts from newly merged current `main`.
- Do not merge BLOCKER/HIGH security or authority findings.
