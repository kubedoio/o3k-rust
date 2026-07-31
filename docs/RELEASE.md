# Release verification

Release bundles are built by `packaging/make-release.sh [version] [profile]`.
The bundle contains `o3kd`, optionally `o3k-compute` for the libvirt profile,
an SPDX 2.3 SBOM, a manifest, and `SHA256SUMS`. The SBOM
and manifest record the source commit and workflow name; local path sources are
represented as `NOASSERTION` so private filesystem paths are not published.

Build and verify a candidate locally:

```bash
SOURCE_DATE_EPOCH=$(git show -s --format=%ct HEAD) packaging/make-release.sh 0.2.0-alpha.1 fake
cd dist/o3k-0.2.0-alpha.1
sha256sum --check SHA256SUMS
python3 -m json.tool sbom.spdx.json >/dev/null
```

The project does not claim SLSA compliance and does not automatically publish
production releases. Before public alpha, the recommended keyless signing
workflow is Sigstore Cosign from a protected GitHub Actions release workflow:

```bash
cosign sign-blob --yes --bundle o3kd.sigstore.json bin/o3kd
cosign verify-blob --bundle o3kd.sigstore.json bin/o3kd \
  --certificate-identity-regexp 'https://github.com/kubedoio/o3k-rust/.github/workflows/.*' \
  --certificate-oidc-issuer 'https://token.actions.githubusercontent.com'
```

Those commands are a proposal until a protected workflow, identity pattern,
and maintainer approval are in place. A release must not be described as
signed merely because it contains checksums.

The libvirt alpha also requires `packaging/release-gate.sh` to report
`status: ready` from real E2E, recovery, clean Ubuntu/Debian installation,
and benchmark artifacts. Missing or skipped evidence blocks the gate.
