# Installing the alpha libvirt profile

Supported alpha hosts are Debian/Ubuntu systems with `systemd`, `openssl`,
QEMU/libvirt, `ip`, `qemu-img`, and (for KVM execution) `/dev/kvm`.

Build and install the versioned profile artifact:

```sh
bash packaging/preflight.sh --profile libvirt
bash packaging/bootstrap-certs.sh --output-dir /etc/o3k/tls
bash packaging/install.sh --profile libvirt --noninteractive
bash packaging/diagnose.sh --profile libvirt
```

The installer creates the `o3k` service account, ownership markers, restrictive
environment files, and separate `o3kd`/`o3k-compute` systemd units. The
compute unit uses the `o3k` user with `libvirt`/`kvm` supplementary groups;
neither service runs as root.

`packaging/reset.sh --yes` preserves credentials and removes only contents of
marked O3K-owned data/log directories. `packaging/uninstall.sh --purge --yes`
similarly refuses unmarked paths and does not touch foreign libvirt domains,
bridges, images, or DHCP configuration. The release script emits SBOM,
checksums, and a source-commit manifest for `fake` or `libvirt` profiles.
