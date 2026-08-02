# OpenStack compatibility matrix

The static release and toolchain target is frozen in
[`SPEC-0016`](../specs/SPEC-0016-static-compatibility-target.md) and its
machine-readable target manifest. It selects OpenStack `2026.1` (Gazpacho) as
primary, retains `2025.2` (Flamingo) as the backward-compatibility profile,
 and pins Rust `1.97.1`; it does not claim that every operation in either
 release is implemented.

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

## Requirements traceability

[`traceability.yaml`](traceability.yaml) links every normative baseline operation
to its capability-inventory record and, where one exists, to a public contract
fixture. It mirrors implementation, portable-contract, and CLI evidence states
from the inventory. The file uses the JSON-compatible subset of YAML so the
dependency-free validator can parse it deterministically with Python's standard
library. Its `protected_runner` field is always `not-claimed`; this artifact does
not create, promote, or assert protected-runner evidence.

Validate the links and the negative mutation case with:

```bash
bash tests/traceability.sh
```

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

## Contract harness

The normative TestLab release contract is frozen in
[`../specs/TESTLAB_API_BASELINE.md`](../specs/TESTLAB_API_BASELINE.md), with
machine-readable rules in
[`../specs/testlab-api-baseline.json`](../specs/testlab-api-baseline.json).
Contract fixtures must reference operations classified as `required` there.

[`contract-fixtures.json`](contract-fixtures.json) contains reviewed,
implementation-neutral public HTTP expectations. Run the same fixtures against
an available target with:

```bash
python3 tests/compatibility-harness.py \
  --target rust \
  --base-url http://127.0.0.1:8774 \
  --source-commit "$(git rev-parse HEAD)" \
  --json-out target/compatibility/rust.json \
  --junit-out target/compatibility/rust.xml
```

Use `--target go` or `--target openstack` for the other public HTTP targets.
The harness reads `OS_AUTH_TOKEN` without printing it, redacts request header
values, normalizes responses to status/header/schema data, and records the
target source revision. `--self-test` exercises the same HTTP runner in CI;
real-target runs are separate portable or protected evidence depending on the
target environment. To compare two targets without changing the expected
contract, use repeated `--compare target=base-url` arguments; agreement is
reported separately from standards compliance.
