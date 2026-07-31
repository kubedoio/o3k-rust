# Real-libvirt testing

The default CI profile is intentionally fake-provider only. A real-libvirt
run must be explicit:

```sh
O3K_TESTLAB_ARTIFACT_DIR=target/testlab-artifacts \
  bash tests/testlab-libvirt.sh
```

The preflight checks `virsh`, `ip`, `/dev/kvm`, and `qemu:///system`. If a
prerequisite is unavailable it writes `libvirt-result.json` with
`"status": "skipped"` and exits successfully; this is not reported as
coverage or a pass. A trusted Linux runner with libvirt/QEMU/KVM, bridge/TAP
permissions, and a small CirrOS image is required for the full lifecycle
harness. Artifacts must be reviewed for redaction before upload.
