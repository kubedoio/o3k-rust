# Real compute-path evidence — Gazpacho Cinder service-testbed

Machine-readable evidence captured from a real protected-run-equivalent local
execution on a KVM/libvirt host. Evidence tiers: `real-compute`,
`component-mock`, `portable`. External Cinder (`real-service`) integration is
deferred; these artifacts prove the O3K-owned compute side of the goal path.

## Artifacts

| Artifact | Tier | Status | What it proves |
| --- | --- | --- | --- |
| `openstack-cli-result.json` | real-compute | passed | Real server create/show/list/stop/start/reboot/console/delete through the public OpenStack CLI against real o3kd; ACTIVE, fixed IP 192.0.2.2, config-drive, console boot marker, verified-absent cleanup |
| `libvirt-result.json` | real-compute | passed | openstack-cli-e2e artifact mirrored as the libvirt profile result |
| `console-result.json` | real-compute | passed | CirrOS boot marker found in guest console (bounded, non-secret) |
| `compute-agent-mtls-result.json` | real-compute | passed | mTLS command acceptance and observation across the agent-provider-to-control-plane-to-agent boundary |

## Environment

- Host: Ubuntu 24.04.4 LTS, kernel 6.8.0-110-generic
- libvirt 10.0.0, QEMU 8.2.2, /dev/kvm present
- O3K provider: agent (real o3k-compute-bin with libvirt feature)
- Guest: CirrOS 0.6.3 (SHA256 7d6355852aeb6dbcd191bcda7cd74f1536cfe5cbf8a10495a7283a8396e4b75b)
- O3K source: main after PR #453/#454/#455

## Provenance

- Generated 2026-08-04 by `scripts/bootstrap-disposable-testlab.sh`
  (`O3K_PROVIDER=agent`) + `tests/testlab-libvirt.sh` +
  `tests/real-compute-agent-mtls.sh`.
- Artifacts originally written under `target/real-host-workflow-artifacts`
  (gitignored); copied here for traceability.

## Remaining (deferred / not yet real)

- `real-service` external Cinder 28.0.0 attach through Nova: deferred.
- `tempest`: `tests/tempest-evidence/tempest-status.yaml` remains NOT_READY.
- Real Nova os-volume_attachments attach of a real Cinder volume: requires the
  external Cinder integration.
