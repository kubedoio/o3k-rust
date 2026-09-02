# P12-IAM.8 — Araf Production-Auth Convergence and P12-IAM Closure

## Goal

Use the completed generic O3K federation contract to unblock Araf production authentication without introducing Araf-specific semantics into O3K.

## Preconditions

- P12-IAM.7 real IdP evidence PASS and merged.
- Araf production-auth branch starts from its current protected `main` separately.
- O3K implementation work in this slice remains limited to genuine contract defects found by the integration; Araf/browser code stays in the Araf repository.

## Required integration boundary

Prove this exact separation:

```text
Browser
 -> Araf Tenant or Operator BFF
 -> OIDC Authorization Code + PKCE / confidential-client handling
 -> external IdP access token kept server-side
 -> O3K federated exchange
 -> native O3K scoped token kept server-side
 -> canonical /identity/me AuthContext
 -> O3K native API
```

O3K must not receive or own Araf browser cookies. Araf must not invent principal/scope/role truth.

## Tenant journey

At minimum prove through the real Araf BFF against real O3K federation:

- browser login redirects to real IdP;
- callback is handled by Tenant BFF;
- no reusable IdP/O3K access token is stored in browser-readable storage;
- scope discovery comes from O3K;
- Project A selection produces Project A native O3K token/AuthContext;
- Araf loads a real O3K tenant resource using that context;
- cross-project access is denied by O3K;
- logout/session expiry stops further authenticated cloud calls.

## Operator journey

Prove:

- separate Operator BFF/OIDC client/session boundary;
- explicitly authorized operator receives system/operator AuthContext;
- normal tenant cannot use Operator API even with route knowledge;
- operator privilege does not derive from Araf application selection alone.

## Security proof

Inspect browser storage/cookies/network traces and BFF logs for:

- no localStorage/sessionStorage/IndexedDB reusable cloud tokens;
- HttpOnly/Secure/SameSite cookie behavior per Araf architecture;
- CSRF protections remain effective;
- token/client-secret redaction;
- tenant/operator cookie isolation;
- O3K failure responses remain RFC 9457 and non-enumerating where required.

## Upstream gap closure

Compare against the Araf blocker/evidence document that triggered P12-IAM. Every production identity gap must be classified:

- RESOLVED;
- ACCEPTED BOUNDED DEVIATION;
- RELEASE BLOCKER.

Do not mark P12-IAM complete if Araf still requires fixture identity or a hidden BFF IAM authority for the supported production path.

## O3K regression proof

Re-run relevant:

- native password/token IAM tests;
- Keystone compatibility identity tests;
- cross-project native API tests;
- system/operator authorization tests;
- SQLite/PostgreSQL identity persistence tests;
- federation real-provider gate.

## Closure artifact

Create a final P12-IAM evidence document under the repository's accepted evidence/docs location containing:

- final O3K main SHA;
- P12-IAM.0–.8 status;
- exact real IdP profile;
- machine-readable contract paths;
- tenant/operator positive and negative evidence;
- restart/key-rotation evidence;
- Araf integration evidence references;
- remaining bounded deviations;
- exact gate commands/results;
- BLOCKER/HIGH/MEDIUM/LOW findings.

## Final verdict

Exactly:

```text
P12-IAM aggregate verdict: PASS|BLOCKED
Araf production identity unblocked: YES|NO
```

`PASS / YES` requires real evidence. Closing issues or compiling code is not sufficient.
