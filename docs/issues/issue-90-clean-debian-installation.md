# Issue #90 — Clean Debian installation and TestLab lifecycle

Issue #90 remains host-gated. This change closes one deterministic uninstall
boundary without claiming that a clean Debian run or real TestLab lifecycle
has passed.

## Bounded repository implementation

`packaging/uninstall.sh --purge` now validates every target's absolute path
and O3K ownership marker before stopping, disabling, or reloading systemd.
An invalid or foreign target therefore cannot cause a service-state mutation.
The behavior is covered by a portable fake-`systemctl` regression test.

## Explicit non-goals

- no Debian package installation or host execution;
- no real libvirt, CirrOS, systemd, reset/reinstall, or lifecycle evidence;
- no claim that `clean-debian-install.json` exists or reports `passed`;
- no change to the documented default-layout service boundary or purge data policy.

Decision: [ADR-0097](../adr/ADR-0097-uninstall-precondition-order.md).
