# get.o3k.io endpoint — Cloudflare Worker artifact (OPTIONAL, non-authoritative)

> **This Worker is NOT required for installation and is NOT a trust
> dependency.** The production path is a Cloudflare Redirect Rule that 302s
> `get.o3k.io` directly to the tagged GitHub Release asset — see
> [`cloudflare-redirect.md`](cloudflare-redirect.md) for the exact
> configuration. GitHub Releases is the authoritative distribution source.
> This Worker is retained only for optional future channel/version
> functionality and may be bypassed entirely; installation works identically
> through the direct GitHub URL.

This directory is a complete, independently deployable Cloudflare Worker (ES
module, no framework, no KV) that serves the one-line O3K TestLab installer
described in [`docs/INSTALLER.md`](../../docs/INSTALLER.md). The Worker source
lives in this repository so the served content is reviewable and versioned
with the code.

## Route map

| Route | Behavior |
|---|---|
| `GET /` | `packaging/get-o3k.sh` verbatim (no redirect) |
| `GET /install.sh` | `packaging/get-o3k.sh` verbatim |
| `GET /version` | current advertised version — the `alpha` channel target |
| `GET /channel/<name>` | channel version as plain text; `404` with a message for unknown channels (never a redirect, never `main`/`latest`) |
| `GET /v<version>` | the same script with `O3K_PINNED_VERSION="<version>"` prepended as the first line; `400` unless `<version>` matches the ADR-0130 release-version fence |
| anything else | `404` with a tiny helpful body |

`HEAD` requests return the same headers without a body; other methods are
`405`.

Release archives (`o3k-<version>-linux-x86_64.tar.gz` and its `.sha256`) are
**served by GitHub Releases, never proxied through this worker**. The pinned
script downloads them directly from
`https://github.com/kubedoio/o3k-rust/releases/download/v<version>/`.

## How the channel table works

`packaging/channels.yaml` maps channel names to exact published release
versions (never git branches, never `latest`) and is advisory/future
functionality only:

```yaml
channels:
  alpha: v0.2.0-alpha.2
```

`GET /channel/alpha` returns `v0.2.0-alpha.2` as plain text. The published
installer never consults this table: it is pinned to its own release version
(baked `O3K_INSTALLER_VERSION` in `packaging/get-o3k.sh`), with resolution
precedence `O3K_VERSION` env > `O3K_PINNED_VERSION` (the `/v<version>` first
line) > the baked pin. Adding a `stable` channel later is a one-line table
change plus a deploy — no redesign.

## Keeping assets in sync (single source of truth)

The worker embeds exact snapshots of the repo's
`packaging/get-o3k.sh` and `packaging/channels.yaml` in `src/assets.js`.
That generated file **is committed** — it is the deployable artifact.

```bash
bash packaging/get-o3k-worker/sync.sh          # regenerate src/assets.js
bash packaging/get-o3k-worker/sync.sh --check  # fail when assets are stale
```

CI runs `sync.sh --check`, so a change to the wrapper script or the channel
table without regenerating `src/assets.js` fails in review. Never edit
`src/assets.js` by hand.

## Testing

```bash
node --test packaging/get-o3k-worker/test.mjs
```

Covers: `/install.sh` matches `packaging/get-o3k.sh` byte-for-byte; `/version`
returns the alpha target; `/channel/alpha` returns `v0.2.0-alpha.2`; unknown
channel is `404`; `/v0.2.0-alpha.2` starts with the exact pin line and then
equals the script; `/v/bogus` is `400`.

## Deployment

```bash
cd packaging/get-o3k-worker
npx wrangler login            # once per operator
npx wrangler deploy           # preview on *.workers.dev
```

For production `get.o3k.io`: prefer the Cloudflare Redirect Rule in
[`cloudflare-redirect.md`](cloudflare-redirect.md). If the Worker's optional
routes are deployed on the domain, configure the owning Cloudflare account
(fill `account_id` in `wrangler.toml` or let wrangler resolve it
interactively) and uncomment the `routes` custom-domain block. Publishing to
the live `get.o3k.io` domain is a release step, not part of this repository's
tests.
