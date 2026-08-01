# Issue #89 — Clean Ubuntu installation and TestLab lifecycle

Issue #89 remains host-gated. This change closes one deterministic packaging
boundary without claiming that a clean Ubuntu run or real TestLab lifecycle
has passed.

## Bounded repository implementation

The installer now rejects relative, root, lexical dot-component,
symlink-component, and non-directory-compatible installation paths before
creating any prefix or owned state directories. The
libvirt profile validates its complete TLS input set and agent fingerprint
before filesystem publication. Release-bundle and packaging safety tests
verify that invalid inputs do not leave a partial installation.

Libvirt preflight now receives the configured `--data-dir` and checks free
space on that filesystem (or its nearest existing ancestor), rather than
always checking `/var/lib`. Malformed or low-space evidence still fails closed.
This is documented in [ADR-0132](../adr/ADR-0132-preflight-data-filesystem.md)
and covered by a custom-path fake-`df` regression.

## Explicit non-goals

- no Ubuntu package installation or host execution;
- no real libvirt, CirrOS, systemd, reset/reinstall, or lifecycle evidence;
- no claim that `clean-ubuntu-install.json` exists or reports `passed`;
- no broad installer transaction/rollback mechanism after input validation.

Decisions: [ADR-0096](../adr/ADR-0096-clean-install-input-validation.md) and
[ADR-0112](../adr/ADR-0112-clean-install-path-component-fence.md).
