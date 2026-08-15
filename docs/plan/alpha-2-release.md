# Release v0.2.0-alpha.2 — install.sh first-class release asset (agent plan)

- Issue: [#615](https://github.com/kubedoio/o3k-rust/issues/615)
- Full specification: `/tmp/p1-link.md` (this plan is the implementation record).
- Deployment/evidence profile: libvirt TestLab alpha, `o3k-implemented` authority mode; supported targets unchanged (Ubuntu 24.04 x86_64, Debian 12 x86_64).
- Authority model: GitHub Releases is the authoritative distribution source. `get.o3k.io` is a convenience 302 redirect only. The installer (`packaging/get-o3k.sh` exported byte-for-byte as `install.sh`) stays a thin bootstrap: OS/arch guard, minimal deps, tagged-tarball download, published-SHA256 verify-before-extract, safe extraction, bundled verification, then the authoritative hardened packaging scripts from inside the verified archive. All security-sensitive logic stays in the archive (preflight/bootstrap-certs/install.sh/bootstrap-testlab.sh/uninstall.sh/reset.sh).
- Public Go O3K reference: not needed for this milestone (no new UX to consult); ADR-0151 record from #614 remains: commit `53fd2cb36ee79f42da49c8181d6ceed12b41b3aa`, UX-only, nothing copied.

## Design decisions

1. **Single installer source.** `packaging/get-o3k.sh` remains the only source. Release generation exports it byte-for-byte to `dist/install.sh` (mode 0755); `cmp` + recorded SHA256 make drift impossible to miss. No second edited copy.
2. **Pinned installer.** The source file carries `O3K_INSTALLER_VERSION=v0.2.0-alpha.2` as the baked default, updated in each release's version-bump commit. The installer never consults a channel service. `O3K_VERSION`/`O3K_RELEASE_BASE` remain as documented dev/test overrides only; production invocation is deterministic.
3. **Channels/Worker.** `packaging/channels.yaml` and the Worker are retained for future channel/version functionality only; their README/docs state plainly they are NOT part of the trusted installation path. A Cloudflare Redirect Rule config artifact documents the exact redirect (/, /install.sh, /v0.2.0-alpha.2 -> tagged asset), 302, enabled only after the release exists.
4. **Authenticity.** Same-chain SHA256 + HTTPS + SSH-signed tag (v0.2.0-alpha.1 model: ssh-ed25519, tagger Senol Colak) + bundle verification. No new signing scheme; docs state exactly what is verified.
5. **Release assets for v0.2.0-alpha.2:** install.sh, o3k-0.2.0-alpha.2-linux-x86_64.tar.gz, o3k-0.2.0-alpha.2-linux-x86_64.tar.gz.sha256, o3kd, o3k-compute, SHA256SUMS, sbom.spdx.json, manifest.json. Manifest records the install.sh SHA256.
6. **Certification then publish, no rebuild after certification** (frozen merged-main SHA, clean checkout, Debian-12 baseline binaries).

## Files expected to change (PR)

- Cargo.toml + Cargo.lock (workspace version 0.2.0-alpha.2; all crates inherit)
- packaging/get-o3k.sh (baked default version; channel fetch removed; override docs)
- packaging/make-release.sh (export dist/install.sh + manifest installer_sha256; keep tarball naming `-linux-x86_64`)
- packaging/make-release-archive.sh (unchanged naming contract; verify it records/checks install.sh too if it lists release assets)
- packaging/channels.yaml (alpha -> v0.2.0-alpha.2)
- packaging/get-o3k-worker/ (assets.js re-sync; README reframed as non-authoritative; add cloudflare-redirect rule artifact)
- tests/installer-negative.sh (replace channel-endpoint cases with pinned-version model; add install.sh export drift/byte-identity cases)
- tests/packaging-bundle.sh (bundle now ships install.sh export)
- .github/workflows/ci.yml + tests/ci-workflow.sh (drift/identity check wired)
- docs: README.md, docs/INSTALLER.md, docs/releases/v0.2.0-alpha.2.md (new), docs/plan/one-line-installer.md (pointer), docs/PACKAGING.md as needed

## Contracts/specs affected

No normative changes. ADR-0130 version fence unchanged; release asset naming follows the existing `-linux-x86_64` convention; docs/RELEASE.md gets a short "install.sh release asset" note. Anything deviating from an ADR/spec gets an issue, not a silent change.

## Required evidence tier

- Portable: negative matrix + packaging tests + worker tests + CI (drift check).
- Candidate-bound real-host certification (this machine, nested KVM): public real-libvirt E2E, failure/recovery matrix, leak/foreign-state verification, clean Ubuntu 24.04 + Debian 12 installs (candidate artifacts, harness per ASR-022 conventions), benchmark, candidate evidence manifest, security regression suite, release gate.
- LIVE public-Internet acceptance (after publication): fresh Ubuntu 24.04 + Debian 12, `curl -sfL https://get.o3k.io | sudo sh -` AND the direct GitHub URL, full matrix (reboot, idempotency, uninstall/reinstall/purge, foreign state). No local endpoint shim.

## Known external blockers (recorded, not silent)

1. SSH signing key for the tag (v0.2.0-alpha.1 was signed by Senol Colak's ssh-ed25519 key — not present in this environment).
2. Cloudflare account access for the get.o3k.io redirect rule.
3. Human-review artifact approval for the new candidate (release-gate requires --human-review approved).

The goal's own order (version PR -> candidate -> certification -> sign/publish -> redirect -> live acceptance) is followed; each external step is blocked only at its position in the chain, with everything else completed first.

## Non-goals

No new runtime features; v0.2.0-alpha.1 untouched; no /latest/ advertisement for alpha; no SLSA/cosign claims without an accepted scheme; Cloudflare never a trust dependency.
