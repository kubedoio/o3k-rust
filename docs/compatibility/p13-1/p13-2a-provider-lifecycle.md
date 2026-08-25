# P13.2A provider lifecycle gate

This gate is bounded to the P13.2A keypair and network resources. It uses the
unmodified OpenTofu 1.12.6 engine and `terraform-provider-openstack/openstack`
3.4.0. The manifest at `provider-toolchain.json` is the checksum authority.

From a fresh shell, bootstrap the pinned Linux amd64 tools and export their
paths:

```bash
eval "$(scripts/p13_2_provider_tools.sh /absolute/path/to/p13-tools)"
```

Build the disposable O3K daemon, verify the checksums, then run the gate:

```bash
cargo build -p o3kd
python3 scripts/p13_provider_contract.py --verify-tools
O3K_P13_O3KD="$PWD/target/debug/o3kd" \
O3K_P13_EVIDENCE_OUTPUT="$PWD/docs/compatibility/p13-1/p13-2a-provider-lifecycle-evidence.json" \
tests/p13_2_core_lifecycle.sh
```

The harness creates a fresh run ID, binds evidence to the current O3K HEAD,
and captures a redacted trace from that same daemon execution. It verifies the
OpenTofu version independently of the `tofu` filename and records the provider
SDK identity separately from the execution engine. A `Terraform/*` CLI run
cannot satisfy the OpenTofu version check. Raw daemon traces remain under the
ignored temporary/target evidence path; only the sanitized summary is suitable
for review or commit.
