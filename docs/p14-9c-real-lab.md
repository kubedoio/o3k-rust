# P14.9C real acceptance lab

P14.9C uses a disposable real two-cloud testbed.  The source is an isolated
Ubuntu Jammy libvirt guest running the upstream DevStack `stable/2025.1`
development deployment.  The destination is the native O3K TestLab
composition on the dedicated acceptance host, with a separate PostgreSQL
database, network-agent state, libvirt compute-agent state, and LVM volume
namespace.  The host runs the P14 runner and OpenTofu.

This is a development/evidence profile, not a production deployment claim.
OpenStack remains the source authority; O3K owns destination state after the
durable cutover commit.  The runner uses `MigrationRunner` for migration
semantics and only supplies real source/destination control and observation
adapters.

## Recreate the source

On a root host with KVM and libvirt:

```sh
scripts/p14_9c_openstack_source.sh create
scripts/p14_9c_openstack_workload.sh create
```

The scripts create the run-owned `p14-openstack-source` VM, an Ubuntu cloud
image verified against the published SHA-256 manifest, the upstream DevStack
checkout, two Keystone projects, and the Project A/Project B workload.  The
workload script attaches a real Cinder volume, writes a deterministic payload
through the guest, verifies its guest-side SHA-256, and records only the
non-secret digest and IDs in the protected environment file.  Credentials,
SSH keys, cloud-init files, and inventories remain below
`O3K_P14_9C_SOURCE_STATE` (default `/var/lib/o3k/p14-9c-source`) with private
permissions.

The source VM and workload are owned by this profile.  `cleanup` removes only
the named libvirt VM and network; it does not search by display name for
unrelated resources.

## Run the acceptance profile

Build the workspace from the protected checkout, provision a fresh isolated
O3K destination database and provider state, and export the runtime values
from the protected source environment.  The destination must use separate
PostgreSQL, compute-agent, network-agent, and storage state.  Then run:

```sh
p14-acceptance prerequisites
p14-acceptance execute
```

The process writes evidence to a protected run directory selected with
`O3K_P14_9A_EVIDENCE_OUTPUT`.  Secrets are runtime inputs only and are not
valid evidence fields.  The evidence validator must report the exact runtime
HEAD, all twenty observed gates, an empty owned-leak count, and a no-op final
OpenTofu plan.

The accepted toolchain is OpenTofu 1.12.6 and the unmodified
`terraform-provider-openstack` 3.4.0 binary.  Hashes are recorded in the
protected run inventory rather than committed to the repository.

## Provenance and boundaries

- OpenStack source: upstream DevStack documentation and repository,
  `stable/2025.1`, with Keystone, Nova, Neutron, Glance, Placement, and
  Cinder using the local development backends.
- OpenStack image: Ubuntu Jammy cloud image and CirrOS 0.6.3 release artifact,
  each checked against the publisher's SHA-256 value.
- Destination: canonical O3K APIs and the existing libvirt, network, and LVM
  execution providers; no direct destination database writes are used by the
  migration runner.
- IaC: OpenTofu 1.12.6 with the unmodified OpenStack provider 3.4.0, per the
  P13 compatibility profile.

The profile deliberately does not add HA, external Ceph, Horizon, Swift,
Heat, telemetry, or a second migration implementation.
