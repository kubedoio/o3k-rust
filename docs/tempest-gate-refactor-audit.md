# Tempest determinism and real-Cinder lifecycle decoupling — audit note

Date: 2026-08-07
PR: "Make Tempest deterministic and decouple it from real-Cinder lifecycle acceptance"
Related: issues #420, #421, #424, #429, #432, #435; PR #501.

## 1. What is already proven (source-bound)

- Real Cinder 28.0.0 (Gazpacho) attachment create, attachment update,
  os-complete, Cinder-reported attached state, CHAP-authenticated iSCSI
  session, o3k-compute attach, libvirt hotplug, detach -> available, cleanup,
  unchanged foreign state (protected runs; deterministic gates A/B/C/D/E from
  PR #501).
- Identity: service-user auth, public token validation, catalog discovery of
  the external volumev3 endpoint.
- Cinder and Tempest never shared a working environment for execution; the
  protected run's Tempest discovery failed (see section 3).

## 2. What is still genuinely unproven

- Tempest execution of the corrected allowlist against the live profile (Gate C
  run result). The allowlist that existed on `main` referenced test IDs that do
  not exist at the pinned tempest 46.0.0 / cinder-tempest-plugin 1.21.0.
- Guest-level block-device consumption (in-guest marker). Deliberately
  diagnostic-only; ownership in #435.

## 3. Product failures vs test-harness failures (determination)

Product (O3K/spec) findings — NOT fixed by this PR, recorded for the profile:

- Tempest compute attachment tests (`AttachVolumeTestJSON.*`) require a
  generic server-create fixture that passes a bare network UUID in the NIC
  reference. The accepted O3K Nova surface validates NIC references as durable
  PORT UUIDs only. Making these tests green would expand the product; per the
  audit rules they are excluded and documented
  (`tests/tempest-evidence/tempest-test-audit.yaml`).
- Token-revocation tests require `DELETE /v3/auth/tokens`, which the identity
  profile lists as unsupported; excluded.

Test-harness failures found and fixed:

- Tempest was installed into the Cinder venv AND a dedicated venv (partial
  isolation), and the subset resolved `tempest`/`stestr` through ambient PATH
  that could select the Cinder venv.
- The allowlist referenced `VolumesV3Test`, `AttachmentsV3Test`,
  `AttachVolumeTest`, `test_token_create_delete`, `test_token_expired_validation`,
  `test_volume_list_details` — none of which exist at the pinned versions.
- `python -m subunit2junitxml` does not work (module is only a console script)
  and the required `junitxml` package was never installed, so the
  subunit -> JUnit conversion silently produced nothing.
- `python -m tempest` does not exist at tempest 46 (`tempest` has no
  `__main__`); `tempest init` must use the console script.
- stestr emits `module.Class.method[id-<uuid>[,tags]]`; the run filters and
  discovery checks must account for the parameterized suffix.

## 4. Work moved off the protected runner (Gate A — portable)

- Dedicated Tempest venv creation, exact version pins (46.0.0 / 1.21.0),
  stestr/subunit tooling, tempest import, `.stestr.conf` + `tempest.conf`
  validity, allowlist existence (import introspection), stestr discovery > 0,
  and the subunit -> JUnit -> summary evidence pipeline are all proven by
  `tests/tempest-preflight.sh` in ordinary CI (no KVM/libvirt/real Cinder/
  MariaDB/RabbitMQ/LVM/tgt).
- Zero-test execution can never be reported as useful evidence.

## Final audit

```text
Before:
  protected runs spent cycles discovering Tempest installation, CLI, Python
  dependency, configuration, test-ID, shell-parsing, and evidence-generation
  problems; Tempest ran (or did not) from the wrong environment with test IDs
  that did not exist, and the JUnit pipeline silently produced no evidence.

Changed:
  - Cinder and Tempest are in separate explicit virtualenvs; the protected
    runner never executes Tempest from the Cinder venv.
  - tests/tempest-preflight.sh proves environment/version/config/discovery/
    evidence in ordinary CI.
  - Canonical allowlist (tests/tempest-evidence/tempest-allowlist.txt) contains
    only tests that exist at the pinned versions and fit the accepted profile.
  - tests/tempest-evidence/tempest-test-audit.yaml records the fixture/API
    dependency graph for every allowlisted test and every excluded candidate.
  - tempest-summary.py is the single JUnit -> summary converter and rejects
    zero/malformed evidence.
  - Gate B (real-Cinder lifecycle) and Gate C (Tempest) verdicts are separate;
    a Tempest harness error cannot invalidate a successful lifecycle.
  - Fragile text/grep assertions replaced with structured parsing: token
    catalog JSON, attachment-status JSON, libvirt domain XML, run-owned iSCSI
    session matching.
  - Guest-level observation recorded as passed | not-proven, never fabricated.

Proven without protected runner:
  tempest 46.0.0 + cinder-tempest-plugin 1.21.0 install in a dedicated venv;
  exact versions; stestr/subunit/JUnit tooling; valid config; all allowlisted
  tests import and are discoverable by stestr; evidence pipeline produces
  valid structured output; zero-test/malformed evidence rejected.

Still requiring protected runner:
  - Gate B: full real-Cinder lifecycle on the exact source commit.
  - Gate C: executing the corrected allowlist against the live profile and
    producing JUnit + summary evidence.

Expected next protected run:
  a verification run proving Gate B still passes and Gate C executes the
  allowlisted tests (identity token issuance + external volume CRUD/list)
  against the live profile, with independent lifecycle and Tempest verdicts.

Issues eligible to close if that run passes:
  - Real-Cinder lifecycle portions of #420/#421/#429 (attach path evidence).
  - #424 Tempest-compatibility evidence for the allowlisted subset (preflight
    already proven in ordinary CI).
  #432 should be updated with the reconciled status. #435 owns guest-level
  consumption proof.
```

See also `docs/status/current-state.yaml` (authoritative explicit states) and
`docs/external-cinder-goal-reconciliation.md` (superseding current status).
