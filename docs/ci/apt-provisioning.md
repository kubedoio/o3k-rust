# CI package provisioning

GitHub-hosted workflows use `scripts/ci/apt-provision.sh` for package
installation. The helper renders an isolated source list containing Ubuntu
repositories and only domains explicitly selected with
`O3K_CI_APT_REQUIRED_DOMAINS`; runner-image repositories such as Google Chrome
are therefore excluded from metadata refresh. Apt's normal signature and hash
verification remains enabled, and required-source failures remain fatal.

| Workflow | Packages | Source policy |
| --- | --- | --- |
| `ci.yml` | protobuf, libvirt, pkg-config, YAML, QEMU, SQLite, tgt, unzip, Python venv | Ubuntu only |
| `component-cinder-mock.yml` | protobuf, libvirt, pkg-config, YAML, QEMU | Ubuntu only |
| `deep-evidence.yml` | protobuf, QEMU, libvirt, pkg-config, Python | Ubuntu only |
| `real-host-validation.yml` | unzip | Ubuntu only |
| `real-lvm-guest.yml` | virtinst, genisoimage | Ubuntu only |
| `real-ceph-rbd-guest.yml` | ceph-common, virtinst, genisoimage | Ubuntu only |

The standalone host/bootstrap scripts also contain apt calls, but run against
the operator-selected target host rather than GitHub-hosted runner images;
they are intentionally not routed through the CI helper.
