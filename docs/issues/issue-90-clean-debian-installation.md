# Issue #90 — Clean Debian installation and TestLab lifecycle

Issue #90 remains host-gated. This change closes one deterministic uninstall
boundary without claiming that a clean Debian run or real TestLab lifecycle
has passed.

## Bounded repository implementation

`packaging/uninstall.sh --purge` now validates every target's absolute path
and O3K ownership marker before stopping, disabling, or reloading systemd.
An invalid or foreign target therefore cannot cause a service-state mutation.
The behavior is covered by a portable fake-`systemctl` regression test.

The preflight disk-space check also fails closed when `df` produces no
parseable capacity row, rather than treating missing evidence as sufficient
free space. A portable fake-`df` regression test covers this boundary.

The uninstall helper now applies the install path fence to `--prefix` and all
purge targets before any removal or ownership check. Relative, root, lexical
dot-component, and symlink-component paths are rejected, including for
non-purge uninstall, so binary/helper cleanup and purge ownership checks cannot
be redirected outside the requested installation layout. The packaging safety
test covers dot- and symlink-component targets.

## Explicit non-goals

- no Debian package installation or host execution;
- no real libvirt, CirrOS, systemd, reset/reinstall, or lifecycle evidence;
- no claim that `clean-debian-install.json` exists or reports `passed`;
- no change to the documented default-layout service boundary or purge data policy.
- no Debian-host disk-space, package, or lifecycle acceptance claim.

Decision: [ADR-0097](../adr/ADR-0097-uninstall-precondition-order.md).
The preflight boundary is recorded in [ADR-0111](../adr/ADR-0111-preflight-disk-space-evidence.md).
The prefix boundary is recorded in [ADR-0120](../adr/ADR-0120-uninstall-prefix-path-fence.md).
