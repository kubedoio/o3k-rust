# Issue #88 — Resource-leak and foreign-state guard

Issue #88 remains host-gated. This change closes one repository-side evidence
gap without claiming that the full real-host verifier has run.

## Bounded repository implementation

The protected workflow inventory now:

- takes two consecutive canonical snapshots and fails closed if the host is
  changing while it is being observed;
- treats requested OpenStack inventory without credentials as unavailable
  instead of accepting an empty resource set;
- publishes inventory JSON atomically, preventing a partial write from being
  consumed as evidence;
- records O3K-owned domain/provider identities for the currently exposed
  public resource APIs, while representing foreign
  domain and non-O3K link state only as redacted SHA-256 digests;
- records validated `o3k-*` network-link identities so leaked TAPs or bridges
  are included in the owned-resource delta;
- hashes the explicit newline-separated `O3K_REAL_HOST_PROTECTED_PATHS`
  allowlist, including descendants and file contents, without publishing raw
  paths or contents; missing, unreadable, or oversized entries fail closed;
- fails the post-run guard when a foreign-state digest changes; and
- writes `resource-leak-result.json`, with `status: passed` reserved for a
  clean owned-resource delta, unchanged foreign digests, and a passed
  lifecycle.

Regression coverage uses stateful fake commands to exercise unstable
collection, redaction, owned-resource leakage, foreign link mutation, and
protected-path mutation.

The protected workflow supplies this required allowlist from the
`O3K_REAL_HOST_PROTECTED_PATHS` environment variable (configured as an
environment variable in the protected GitHub environment). An unset allowlist
is rejected rather than treated as an empty clean baseline.

## Explicit boundary

The complete issue acceptance still requires an independent real-host
inventory around both the normal E2E and failure-injection suites, including
TAPs, DHCP state, protected filesystem paths, ports, Placement allocations,
operations, temporary files, and processes. No real-host evidence is claimed.

Decision: [ADR-0095](../adr/ADR-0095-race-safe-resource-leak-evidence.md).

Keypair inventory remains a follow-up because the current O3K public API does
not expose a keypair endpoint; the guard does not treat that unsupported API
as an empty clean inventory.
