# One-line O3K TestLab installer — agent plan

- Issue: [#613](https://github.com/kubedoio/o3k-rust/issues/613)
- Full specification: goal file circulated as `/tmp/p1-goal.md` (same content as the issue body; this plan is the implementation record, not a redefinition).
- Deployment/evidence profile: libvirt TestLab alpha, `o3k-implemented` authority mode.
- Canonical O3K service/domain: Cloud Kernel control plane (`o3kd`) + `o3k-compute` execution provider. No new daemons, no new OpenStack API surface.
- OpenStack compatibility adapter: unchanged; TestLab bootstrap consumes public v3 APIs via `python3-openstackclient` only.
- Public Go O3K reference (UX only, ADR-0151, do not copy): pinned commit `53fd2cb36ee79f42da49c8181d6ceed12b41b3aa`, local checkout `target/go-o3k-reference/`, inspected paths `scripts/install.sh`, `scripts/bootstrap.sh`, `docs/INSTALLATION.md`. Its netplan/systemd-networkd/NAT/database behavior is explicitly out of scope.

## Design

```
get.o3k.io (Cloudflare Worker, deployable artifact in-repo)
  GET /              -> packaging/get-o3k.sh verbatim (no redirect)
  GET /install.sh    -> packaging/get-o3k.sh (small)
  GET /version       -> current advertised version
  GET /channel/alpha -> v0.2.0-alpha.1 (channel table in packaging/channels.yaml)
  GET /v<version>    -> same wrapper, version pinned via URL path
                       (O3K_PINNED_VERSION first line; 400 unless the version
                       matches the ADR-0130 fence)
```

`get-o3k.sh` (thin orchestrator, no reimplementation of packaging ownership):

1. require Linux + x86_64 + Ubuntu 24.04 / Debian 12 (else clear failure);
2. private `mktemp -d` temp dir, `trap` cleanup, never execute unverified content;
3. resolve version (`O3K_VERSION` > endpoint-injected pin `O3K_PINNED_VERSION` > channel alpha); never fall back to main;
4. apt install the minimal certified dependency set (ca-certificates, curl, libvirt-daemon-system, libvirt-clients, qemu-kvm, qemu-utils, iproute2, dnsmasq-base, polkitd, genisoimage, python3, python3-openstackclient);
5. download `o3k-<version>-linux-x86_64.tar.gz` + published `.sha256` from GitHub Releases (base overridable via `O3K_RELEASE_BASE` for testing/campaigns);
6. verify published SHA256 **before** extraction; abort on any mismatch;
7. safe extract (no absolute paths, no `..`, no symlink escape);
8. run bundled `packaging/verify-release-bundle.sh`; then `packaging/preflight.sh --profile libvirt`;
9. run bundled `packaging/bootstrap-certs.sh` only when the complete TLS set is absent (never regenerate valid identities, never `--force`);
10. run bundled `packaging/install.sh --profile libvirt --noninteractive --binary <verified o3kd> --compute-binary <verified o3k-compute>`;
11. client credentials `/etc/o3k/admin-openrc` + `/etc/o3k/clouds.yaml` (0600, ledger-owned — generated inside install.sh, not a second source of truth);
12. run bundled `packaging/bootstrap-testlab.sh` (public APIs only, idempotent);
13. print the §15 UX checklist. No secrets in output.

Release publishing: new `packaging/make-release-archive.sh` (or extension of make-release.sh) produces the tarball + published SHA256 file; both are the GitHub Release assets the wrapper downloads. `packaging/channels.yaml` maps channels to versions.

## Files expected to change

- created (wrapper/bundle slice): `packaging/get-o3k.sh`, `packaging/bootstrap-testlab.sh`, `packaging/channels.yaml`, `packaging/make-release-archive.sh`
- created (endpoint/test/doc slice): `packaging/get-o3k-worker/` (Worker source `src/index.js`, generated snapshot `src/assets.js`, `sync.sh` with `--check`, `wrangler.toml`, `README.md`, `test.mjs`), `tests/installer-negative.sh`, `docs/INSTALLER.md`
- modified: `packaging/install.sh` (client credential files + ledger), `packaging/uninstall.sh` (purge coverage of those files if needed), `packaging/make-release.sh` (bundle copy list), `README.md` (quick-install section), `docs/PACKAGING.md` (cross-links + script inventory), `docs/cirros-walkthrough.md` (pointer to the automated path), `.github/workflows/ci.yml` + `tests/ci-workflow.sh` (new CI steps and contract asserts)
- no changes to: Rust crates, OpenStack API routes, security posture of `o3kd`/`o3k-compute`, reset/uninstall/purge fences (only additive ledger entries for the two new credential files)

## Contracts/specs affected

- No normative contract changes. New artifacts must respect: ADR-0112/0139 (owned-file markers), ADR-0120/0131 (uninstall/reset fences), ADR-0130 (version format), ADR-0151 (Go reference), SPEC-0022/0024 evidence gates. Any deviation gets an issue, not a silent change.

## Required evidence tier

- portable negative matrix + packaging tests (this machine) — done: `tests/installer-negative.sh` (65 cases), `packaging-safety.sh`, `packaging-bundle.sh`, worker tests, CI-workflow asserts;
- real acceptance: fresh Ubuntu 24.04 and Debian 12 nested-KVM VMs driven through the one-liner (campaign harness reuses the ASR-021 `vm-run.sh` VM provisioning shape; new phased in-VM scripts because the campaign requires a HOST REBOOT step the old single-shot script cannot do):
  1. host side: rebuild the release bundle from the working tree (temp git copy so `make-release.sh`'s clean-tree check holds; Debian-12-baseline release binaries, glibc floor checked), run `make-release-archive.sh`, serve a local endpoint shim implementing the exact Worker routes (/, /install.sh, /version, /channel/alpha, /v<version>) plus GitHub-Releases-shaped asset paths on 0.0.0.0 (guest reaches it via slirp gateway 10.0.2.2);
  2. in-VM phase 1: foreign canaries + pre-checks, then the literal one-liner `curl -sfL http://10.0.2.2:<port>/ | sudo env O3K_INSTALL_BASE=... O3K_RELEASE_BASE=... sh -` (production bases are the only override; no repo checkout, no bundle copy), wait for the §15 checklist, then `sudo reboot`;
  3. host waits for SSH + boot_id change; in-VM phase 2: services healthy + agent reconnected + test-vm identity preserved, one-liner re-run (no duplicate resources/identities — assert IDs unchanged), bootstrap-testlab --teardown, uninstall, one-liner reinstall, lifecycle re-run, purge, zero-residue + foreign-canary + leak checks, evidence JSON;
  Note: the review cycle fixed a startup-restore race and the bookworm flavor-ownership guard after the first green runs; binaries/scripts changed, so BOTH campaigns are re-run against the final review-fixed tree before the PR.
- live `https://get.o3k.io` + real GitHub Release assets are an external publication step requiring release approval — tracked explicitly in the final verdict;
- recorded deviation (issue #613 comment): purge intentionally retains the two service accounts per the accepted hardened contract; "zero residue" is proven for all ledger/manifest-owned artifacts, with the accounts as the single documented exception.

## Non-goals

No OpenStack API breadth, PostgreSQL, Kubernetes/Helm, HA, native Cinder, metadata HTTP, CellHV, host-wide NAT/netplan writes, non-x86_64, non-Ubuntu-24.04/Debian-12 platforms.

## Known uncertainties

- exact CirrOS 0.6.3 pinned SHA and download URL (must be recovered from repo records or pinned from the official checksums file);
- interaction of new `/etc/o3k` credential files with the config-file ledger and purge refusal logic (must read install.sh/uninstall.sh before editing);
- whether `o3kd` admin auth needs `O3K_BOOTSTRAP_PASSWORD` or a distinct identity (verify against `crates/o3k-config` + tests).
