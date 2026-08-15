# Installation and packaging

The supported TestLab layout is:

```text
/usr/local/bin/o3kd
/etc/o3k/o3kd.env       # mode 0600, credentials
/var/lib/o3k            # SQLite and owned resource state
/var/log/o3k             # daemon logs
/etc/systemd/system/o3kd.service
/etc/systemd/system/o3k-compute.service   # libvirt profile only
```

Install from a checkout or release bundle with `packaging/install.sh
--noninteractive`. The service runs as the dedicated `o3k` user and uses
systemd hardening. For an unprivileged test installation, pass explicit
`--prefix`, `--data-dir`, `--config-dir`, and `--log-dir` paths.

`packaging/make-release.sh [version] [profile]` builds a versioned fake or
libvirt bundle with a manifest and SHA-256 checksum. Release binaries are
built on the Debian 12 (bookworm, glibc 2.36) baseline with
`scripts/build-release-binaries-debian12.sh` and passed to the bundler via
`O3K_RELEASE_BINARIES_DIR` (see `docs/RELEASE.md` for the build-baseline
requirement and commands). The libvirt profile also
ships `o3k-compute`, preflight/diagnostics, certificate bootstrap, and the
release gate. Release artifacts are not claims of CellHV support; the CellHV
profile remains separately environment-gated.

The version must use the numeric release format with an optional alphanumeric
prerelease suffix (for example, `0.2.0-alpha.2`). Unsafe path or control
characters are rejected before build or output cleanup.

Reset is explicit and preserves credentials:

```text
sudo /usr/local/share/o3k/reset.sh --yes
```

Uninstall preserves state by default. Destruction requires both `--purge` and
`--yes`; the scripts reject empty, relative, root, or unresolved broad paths.

Upgrade and rollback boundaries are package-level: stop the service, retain
the SQLite/data layout, replace the versioned binary, run readiness and the
TestLab workflow, and restore the previous binary if validation fails. Schema
migrations must remain forward-compatible before an upgrade is published.

## One-line installer

For a clean Ubuntu 24.04 / Debian 12 host, the supported entry point is the
one-line installer (`curl -sfL https://get.o3k.io | sudo sh -` — a
convenience 302 redirect to the tagged GitHub Release asset) documented in
[docs/INSTALLER.md](INSTALLER.md). Its packaging artifacts:

- `packaging/get-o3k.sh` — the thin installer wrapper, exported
  byte-for-byte as the `install.sh` GitHub Release asset by
  `packaging/make-release.sh` (drift-gated by `cmp`, SHA-256 recorded in the
  bundle manifest); it is pinned to its own release version
  (`O3K_INSTALLER_VERSION`), never consults a channel service, installs host
  dependencies, downloads and verifies the release archive, then drives the
  bundle scripts below;
- `packaging/channels.yaml` — the advisory channel table
  (`alpha -> v0.2.0-alpha.2`) for future channel/version functionality only;
  embedded in the optional worker (`packaging/get-o3k-worker/`, generated
  snapshot `src/assets.js`, kept in sync by `sync.sh --check`); the
  production `get.o3k.io` path is the Cloudflare Redirect Rule in
  `packaging/get-o3k-worker/cloudflare-redirect.md`;
- `packaging/make-release-archive.sh` — produces the GitHub Release assets
  (`o3k-<version>-linux-x86_64.tar.gz` + published `.sha256`) the wrapper
  downloads, verifies the archive shape before publishing, and re-checks the
  `dist/install.sh` byte-identity gate before archiving;
- `packaging/bootstrap-testlab.sh` — idempotent public-API TestLab bootstrap
  (CirrOS, network, subnet, port, flavor, keypair, `test-vm`, console).
