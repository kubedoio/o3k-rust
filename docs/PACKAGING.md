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
libvirt bundle with a manifest and SHA-256 checksum. The libvirt profile also
ships `o3k-compute`, preflight/diagnostics, certificate bootstrap, and the
release gate. Release artifacts are not claims of CellHV support; the CellHV
profile remains separately environment-gated.

The version must use the numeric release format with an optional alphanumeric
prerelease suffix (for example, `0.2.0-alpha.1`). Unsafe path or control
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
