# ADR-0139 — Preserve foreign installation files

Status: accepted

The installer rejects symlinked or non-directory `PREFIX/bin`, `PREFIX/share`,
and `PREFIX/share/o3k` paths before creating files. It refuses to overwrite an
existing O3K-named file unless a same-prefix `.o3k-installed` manifest proves
that the prior installation owns that relative path.

The manifest records the exact installed files. Uninstall validates its header
and every entry against the fixed installation inventory, then removes only
listed regular files and the manifest. Missing, malformed, or tampered
ownership state fails closed. This preserves foreign files even when they use
an O3K-looking name, at the cost of requiring manual migration for installs
created before this manifest was introduced.

This is a repository safety boundary; clean Debian host installation and
full lifecycle evidence remain host-gated under issue #90.
