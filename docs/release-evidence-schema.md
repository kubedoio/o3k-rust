# Release evidence schema

`packaging/release-gate.sh` accepts only artifacts that identify their purpose;
it does not accept a generic `{"status":"passed"}` file. Every artifact must
be a JSON object with:

- `artifact_type`, `profile: "libvirt"`, `redacted: true`;
- an integer `finished_at` epoch timestamp;
- `cleanup.status: "passed"`;
- the status required by the gate (`passed` for workflows, `measured` for the
  benchmark).

The required artifact types are:

| Gate input | `artifact_type` | Additional proof |
|---|---|---|
| `--e2e` | `openstack-cli-e2e` | `public_api_only: true`; all create/show/list/stop/start/reboot/console/delete lifecycle fields true |
| `--install-ubuntu` | `clean-install` | `distro: ubuntu` and clean-host install result |
| `--install-debian` | `clean-install` | `distro: debian` and clean-host install result |
| `--recovery` | `failure-recovery` | non-empty `failures` list |
| `--benchmark` | `benchmark` | `guest_and_libvirt.status: measured` and all evaluated targets true |

Paths must be distinct. Preflight, skipped, fake-profile, stale, or reused
artifacts are rejected. Test scripts remove their prior result files before
starting so an interrupted run cannot leave a previous pass available to the
gate. The CLI harness also writes a redacted `failed` artifact after a
non-skipped lifecycle error and records the cleanup result; this is diagnostic
evidence only and remains ineligible for the release gate.
