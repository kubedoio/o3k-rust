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

## Repository-side verifier (2026-08-09)

Repository-side tooling for the full issue acceptance now exists without
claiming host evidence:

- `scripts/real-host-owned-inventory.py` schema_version 3 (env-gated
  `O3K_REAL_HOST_STATE_ROOT` / `O3K_REAL_HOST_PID_ROOT` /
  `O3K_REAL_HOST_CANARIES`) adds managed-state, DHCP, process, durable-ledger,
  and canary sections with the issue's classification contract;
- `scripts/real-host-leak-verifier.py` provides `compare`, `negative-stale`,
  `negative-foreign`, and `aggregate`, producing the protected
  `resource-leak-result` artifact (schema_version 2) per ADR-0164;
- `tests/real-host-leak-verifier.sh` exercises both against fake binaries
  and a real sqlite3 ledger.

The real-host run (clean baseline, canaries, normal E2E, and the full
failure-injection matrix under the verifier) remains host-gated and is not
claimed here. Decision: [ADR-0164](../adr/ADR-0164-independent-resource-leak-verifier.md).

## Real-host completion (2026-08-10)

Issue #88 is CLOSED. The independent verifier (ADR-0164, superseding
ADR-0095's bounded inventory) ran on the trusted KVM/libvirt host
(nkudo-vm1) at the exact final source `519517e5bd217b08ba1b5e6957f996040fd8fbac`
around both the normal CirrOS E2E and the complete #87 failure-injection
suite: 22/22 scenario verdicts passed with zero owned leaks,
inconsistencies, and foreign-state changes; the stale-artifact and
foreign-mutation negative tests detected their injections as expected.
The aggregate `target/real-host-workflow-artifacts/resource-leak-result.json`
reports `status: passed`. The verifier caught 12 genuine residue/stuck-state
classes during the campaign; each was fixed test-first and merged
(#563-#579) and re-verified on the real host.

## Explicit boundary

The pre-completion state: the complete issue acceptance required an
independent real-host inventory around both the normal E2E and
failure-injection suites, including TAPs, DHCP state, protected filesystem
paths, ports, Placement allocations, operations, temporary files, and
processes — now executed (see above).

Decision: [ADR-0095](../adr/ADR-0095-race-safe-resource-leak-evidence.md),
superseded by [ADR-0164](../adr/ADR-0164-independent-resource-leak-verifier.md).

Keypair inventory remains a follow-up because the current O3K public API does
not expose a keypair endpoint; the guard does not treat that unsupported API
as an empty clean inventory.
