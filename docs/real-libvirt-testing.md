# Real-libvirt testing

The default CI profile is intentionally fake-provider only. A real-libvirt
run must be explicit:

```sh
O3K_TESTLAB_ARTIFACT_DIR=target/testlab-artifacts \
  bash tests/testlab-libvirt.sh
```

The runner checks `virsh`, `qemu-img`, `ip`, `/dev/kvm`, and `qemu:///system`,
then invokes the public OpenStack CLI lifecycle workflow against the already
installed libvirt profile. If a
prerequisite is unavailable it writes `libvirt-result.json` with
`"status": "skipped"` and exits successfully; this is not reported as
coverage or a pass. Missing CLI credentials or endpoint access is also
explicitly skipped. A trusted Linux runner with libvirt/QEMU/KVM, bridge/TAP
permissions, an installed/configured o3k libvirt profile, and a small CirrOS
image is required for the full lifecycle harness. Artifacts must be reviewed
for redaction before upload.
