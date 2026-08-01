# ADR-0095 — Require stable, redacted resource-leak evidence

## Status

Accepted

## Context

Issue #88 requires an independent guard around real-host workflows. The
existing pre/post guard compared one libvirt/OpenStack snapshot and could
publish a partial or time-of-check snapshot while the host inventory was
changing. It also did not expose the issue's required `resource-leak-result`
artifact or detect changes to unrelated host networking state.

## Decision

The owned-resource inventory is collected twice and is accepted only when two
consecutive canonical snapshots match. Collection failures and an unstable
host fail closed; when OpenStack inventory is requested, missing credentials
are also a collection failure rather than an unchecked empty inventory. JSON
publication uses a same-directory temporary file and
`os.replace`, so an interrupted writer cannot leave a result that looks like a
complete snapshot.

The inventory includes O3K-owned domain and provider identities, including
keypairs created by the public CLI workflow, and validated `o3k-*` network-link
identities so owned TAPs and bridges can be compared across snapshots. Foreign
libvirt domain names and non-O3K link records are represented only by sorted
SHA-256 digests; their raw identities are never written to the artifact. The
foreign-state digest also covers the explicit newline-separated
`O3K_REAL_HOST_PROTECTED_PATHS` allowlist. It hashes path identities,
descendants, metadata, and regular-file or symlink contents without publishing
raw paths or contents; missing, unreadable, or oversized entries fail closed.
post-run guard compares those digests and emits
`resource-leak-result.json`, whose `passed` status is possible only when the
owned-resource delta and foreign-state digest comparison are clean.

## Consequences

- Portable tests cover unstable collection, atomic redacted output, clean
  results, owned domain/network-link leaks, foreign link mutation, and protected-path
  mutation.
- A changing host can produce a blocked/failed guard rather than a false clean
  result; operators must rerun after establishing a quiet baseline.
- The inventory remains intentionally bounded. It does not yet inventory
  every TAP, DHCP lease, filesystem path, port, Placement allocation,
  operation, or process required by the complete issue acceptance.
- No real-host run or leak-free E2E result is claimed by this repository
  change.

## Provenance

This is an independently authored repository decision based on issue #88,
the project release-evidence schema, and the existing protected workflow
contract. No private source or implementation was used.
