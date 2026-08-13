# Release evidence schema

`packaging/release-gate.sh` accepts only artifacts that identify their purpose
and exact candidate provenance. Every invocation must supply
`--candidate-evidence-manifest` alongside the six machine artifacts. The
manifest is a JSON object with `artifact_type: "candidate-evidence-manifest"`,
the 40-character `candidate_sha`, and lowercase SHA-256 digests for
`o3kd_sha256`, `o3k_compute_sha256`, and `bundle_tree_sha256`. Its `artifacts`
map must contain one entry for each gate input, naming the artifact and its
SHA-256 digest. Each machine artifact must contain a matching `source_commit`,
the three binary/bundle digests, and its manifest `artifact_sha256`; missing,
null, stale, or mismatched values block readiness. The gate recomputes all
digests and does not trust summary booleans. This prevents an otherwise valid
human review from making stale evidence release-eligible.

The gate accepts only artifacts that identify their purpose;
it does not accept a generic `{"status":"passed"}` file. Every artifact must
be a JSON object with:

- `artifact_type`, `profile: "libvirt"`, `redacted: true`;
- a positive `finished_at` epoch timestamp that is not in the future and is no
  older than seven days (604800 seconds) when the gate runs. Operators may
  set `O3K_RELEASE_EVIDENCE_MAX_AGE_SECONDS` to another positive integer for a
  controlled gate invocation; leaving it unset retains the safe seven-day
  default;
- `cleanup.status: "passed"`;
- for the CLI workflow, `cleanup.resources` records each created resource's
  disposition as `verified_absent`, `not_verified`, or `pending`;
- the status required by the gate (`passed` for workflows, `measured` for the
  benchmark).

The required artifact types are:

| Gate input | `artifact_type` | Additional proof |
|---|---|---|
| `--e2e` | `openstack-cli-e2e` | `public_api_only: true`; all create/show/list/stop/start/reboot/console/delete lifecycle fields true; `acceptance` must prove initial `ACTIVE`, a non-empty fixed IP, config-drive attachment, a console boot marker, and post-reboot `restart` evidence proving `ACTIVE`, fixed IP, and config-drive attachment; redacted `resources` IDs per the normative resource contract `contracts/release-e2e-evidence.schema.json` |
| `--install-ubuntu` | `clean-install` | `distro: ubuntu` and clean-host install result |
| `--install-debian` | `clean-install` | `distro: debian` and clean-host install result |
| `--recovery` | `failure-recovery` | `scenarios` object contains every required scenario key, each with `status: passed` |
| `--benchmark` | `benchmark` | `guest_and_libvirt.status: measured`; `release_eligible: true`; all evaluated targets true; and `raw_sha256` binding the summary to `--benchmark-raw` |
| `--benchmark-raw` | `benchmark` | `status: measured`; `profile: libvirt`; `redacted: true`; `release_eligible: true`; fresh `finished_at`; non-empty `environment.uname` and `environment.rustc`; positive `samples`; measured `guest_and_libvirt`; and `targets.startup_readiness_ms`, `targets.idle_rss_mib`, `targets.token_p95_ms` |

The gate also requires `--human-review`, `--source-commit`, and
`--candidate-evidence-manifest`. The human
review path must pass
`packaging/validate-human-review.sh --require-approved`, and its
`reviewed_commit` must equal the supplied 40-character lowercase
`--source-commit`. The supplied commit must also equal `git rev-parse HEAD` in
the source checkout running the gate; missing or mismatched checkout provenance
blocks readiness. This is a governance artifact, not host evidence: the gate
does not establish reviewer identity, judgment, signatures, or publication.

The benchmark summary and raw artifact are separate required inputs. Their
`artifact_type`, `status`, `profile`, `samples`, `finished_at`, `control_plane`,
`guest_and_libvirt`, and `release_eligible` values must be identical. Both artifacts' `finished_at`
values are checked against the same gate timestamp and age policy. The summary's `raw_sha256` must
be 64 lowercase hexadecimal characters and equal the SHA-256 digest of the
raw artifact's canonical UTF-8 JSON: object keys sorted recursively, no
whitespace (`separators=(',', ':')`), and non-ASCII characters escaped. This
binds the measured summary to the exact raw document that was reviewed;
changing raw data, even while preserving its valid JSON shape, blocks the
gate. The gate does not use file metadata or paths as the binding.

Paths must be distinct. Preflight, skipped, fake-profile, stale, or reused
artifacts are rejected. Test scripts remove their prior result files before
starting so an interrupted run cannot leave a previous pass available to the
gate. The CLI harness also writes a redacted `failed` artifact after a
non-skipped lifecycle error and records the cleanup result; this is diagnostic
evidence only and remains ineligible for the release gate.

For `openstack-cli-e2e` artifacts, every created resource must be
`verified_absent` when `cleanup.status` is `passed`. The CLI workflow creates
and proves verified-absent cleanup of the image, network, subnet, port,
flavor, keypair, and server. A failed cleanup retains
the owned resource's `not_verified` or `pending` disposition instead of
implying that an unsuccessful delete command established absence. The
machine-readable normative contract for the exact resource membership and
cleanup vocabulary is `contracts/release-e2e-evidence.schema.json`, validated
by `scripts/validate-release-e2e-evidence.py`; this document summarizes that
contract rather than duplicating its key lists.

The protected `resource-leak-result` artifact may expose owned
`leaks.network_links`, containing only validated `o3k-*` interface names. A
new owned link relative to the baseline is a leak; foreign interface names
remain represented only by the redacted `foreign_state` digest. The full
verifier artifact is the output of
`scripts/real-host-leak-verifier.py aggregate` (schema_version 2,
`artifact_type: "resource-leak-result"`, per ADR-0164): owned leaks expose
only validated O3K-owned identities with their classification, foreign state
only digests and operator-chosen canary identities, and the aggregate passes
only when every scope verdict and both negative tests pass.

The gate captures one current epoch timestamp for the invocation and applies
the same timestamp policy to every artifact. A timestamp equal to the maximum
age boundary is accepted; timestamps older than that boundary, non-positive
timestamps, and future-dated timestamps are rejected. The age override is
intended for controlled local or CI runs and must not be used to conceal stale
release evidence.

The failure-recovery artifact must contain these scenario keys under
`scenarios`. Each value is a machine-readable result object whose `status` is
`passed` and whose `evidence` object identifies a non-empty `artifact` and a
non-empty list of `checks`; missing, unknown, non-passed, or evidence-less
scenarios block the gate:

```text
control-plane-crash-before-dispatch
control-plane-crash-after-dispatch
compute-agent-crash-before-mutation
compute-agent-crash-after-domain-definition-or-start
libvirt-daemon-restart
agent-control-plane-network-interruption
timeout-after-accepted-mutation
duplicate-create-delivery
duplicate-action-delivery
duplicate-delete-delivery
corrupted-truncated-image
image-checksum-mismatch
qemu-img-failure
config-drive-failure
tap-failure
dnsmasq-failure
disk-full
repeated-delete
partial-cleanup
```
