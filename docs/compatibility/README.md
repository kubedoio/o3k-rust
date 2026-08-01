# OpenStack compatibility matrix

The compatibility matrix is the authoritative inventory of the OpenStack-facing
behavior O3K intends to support. It records intent and evidence separately:
`planned` means that the operation is in the roadmap, not that O3K currently
implements or supports it.

## Machine-readable format

[`matrix.yaml`](matrix.yaml) is the canonical machine-readable representation.
The top-level `schema_version` is incremented when the structure changes. Each
operation entry contains:

- `service` and `operation`: stable identifiers used by tests and reports;
- `public_sources`: URLs to public OpenStack documentation only;
- `api_version` and optional `microversion`: the compatibility target;
- `supported_fields`: fields O3K intends to implement in the initial profile;
- `known_deviations`: explicit behavior that differs from the target;
- `executable_tests`: repository test identifiers or paths, once available;
- `status`: the evidence state of the operation.

The file is deliberately data-only YAML so a later CI check can parse it without
having to infer behavior from prose or implementation details.

## Status vocabulary

- `planned`: scoped, but no implementation evidence exists;
- `implemented`: code exists, but compatibility evidence is incomplete;
- `verified`: executable compatibility evidence passes for the documented scope;
- `unsupported`: intentionally outside the current profile;
- `superseded`: replaced by a newer matrix entry, which must be linked in the
  entry's `known_deviations`.

Only `verified` entries may be presented as supported compatibility. An entry
must not move to `implemented` or `verified` without adding the corresponding
code/tests and updating `executable_tests`. A newly discovered deviation is
recorded before changing the status. Changes to the target API version,
supported fields, or deviations require a PR review and an entry in the PR's
compatibility section.

## Public-source policy

Every source is a public, stable OpenStack documentation URL. The access date
is recorded in `source_accessed` using ISO-8601 calendar format. Private source
code, internal schemas, credentials, and undocumented behavior are not valid
matrix evidence.

The initial matrix is intentionally marked `planned`; it describes the TestLab
vertical slice from `SPEC-0001` and makes no claim of present OpenStack parity.

## Capability inventory

[`capability-inventory.json`](capability-inventory.json) and
[`capability-inventory.md`](capability-inventory.md) are generated from
[`capability-inventory-source.json`](capability-inventory-source.json) by
[`scripts/generate-capability-inventory.py`](../../scripts/generate-capability-inventory.py).
The inventory is a route and capability baseline, not a compatibility claim.
It independently records official OpenStack operations, public Go reference
locations, Rust locations, CLI commands, implementation state, and each
evidence state. The Go snapshot and Rust snapshot are pinned in the source
manifest; the Go repository remains non-normative under ADR-0151.

Regenerate and verify it with:

```bash
bash tests/capability-inventory.sh
```

Do not promote a route from `implemented` to a verified evidence state without
the corresponding executable contract, CLI, or protected-runner artifact.
