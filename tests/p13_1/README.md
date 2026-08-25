# P13.1A harness

This slice provides the reproducible foundation for real OpenTofu/provider
evidence. It does not emulate OpenStack and does not modify the upstream
provider.

Offline validation:

```bash
tests/p13_1_provider_harness.sh
```

The real gate requires a real OpenTofu project, a running O3K endpoint with
boundary trace output, and an unmodified provider binary:

```bash
O3K_P13_RUN_REAL=1 \
O3K_P13_TOFU_ARCHIVE=/absolute/path/to/tofu_1.12.6_linux_amd64.tar.gz \
O3K_P13_PROVIDER_ARCHIVE=/absolute/path/terraform-provider-openstack_3.4.0_linux_amd64.zip \
O3K_P13_PROVIDER_BINARY=/absolute/path/terraform-provider-openstack_v3.4.0 \
O3K_P13_PROVIDER_SHA256=<release-sha256> \
O3K_P13_TOFU_PROJECT=/absolute/path/to/project \
O3K_P13_RAW_EVIDENCE=/absolute/path/to/redacted-trace.json \
O3K_P13_EVIDENCE_OUTPUT=/absolute/path/to/provider-contract.json \
tests/p13_1_provider_harness.sh
```

`O3K_P13_RAW_EVIDENCE` must be produced by transparent O3K compatibility-boundary
capture. The harness never synthesizes OpenStack responses.

The committed probe project is at `tests/p13_1/real-project`. Set its
`TF_VAR_*` values for the O3K test project before running the real gate. The
provider is downloaded by OpenTofu from the public registry at exactly v3.4.0;
the disposable project prevents generated lock state from changing the
repository.

P13.1B runs the real process, creates one owned disposable image fixture, and
executes both read-only data sources:

```bash
O3K_P13_TOFU_ARCHIVE=... \
O3K_P13_PROVIDER_ARCHIVE=... \
O3K_P13_PROVIDER_BINARY=... \
O3K_P13_PROVIDER_SHA256=... \
O3K_P13_TOFU=/absolute/path/to/tofu \
tests/p13_1b_provider_harness.sh
```

The P13.1B run fails closed if the provider plan fails or if the expected
request sequence is absent. The artifact is written under `target/p13-1b/`
unless `O3K_P13_EVIDENCE_OUTPUT` is supplied.

P13.1C runs five isolated real-provider managed-resource applies. It records
successful partial operations and precise failures without changing O3K runtime
compatibility behavior:

```bash
O3K_P13_TOFU_ARCHIVE=... \
O3K_P13_PROVIDER_ARCHIVE=... \
O3K_P13_PROVIDER_BINARY=... \
O3K_P13_PROVIDER_SHA256=... \
O3K_P13_TOFU=/absolute/path/to/tofu \
tests/p13_1c_provider_harness.sh
```

The P13.1C artifact is written under `target/p13-1c/`. Pinned provider source
provenance is recorded in `provider-managed-resource-source-evidence.md`, and
the resulting P13.2 requirements matrix is in
`docs/compatibility/p13-1/p13-2-managed-resource-requirements.yaml`.
