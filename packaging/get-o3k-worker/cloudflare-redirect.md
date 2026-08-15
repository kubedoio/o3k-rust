# get.o3k.io — Cloudflare Redirect Rule configuration artifact

The production installation path for get.o3k.io is a **Cloudflare Redirect
Rule**, not the Worker in this directory. GitHub Releases is the authoritative
distribution source; get.o3k.io is only a convenience 302 redirect to the
tagged GitHub Release asset. Cloudflare is never a trust dependency.

## Rule

- Host: `get.o3k.io`
- Paths:
  - `/`
  - `/install.sh`
  - `/v0.2.0-alpha.2`
- Redirect target (all three paths):
  `https://github.com/kubedoio/o3k-rust/releases/download/v0.2.0-alpha.2/install.sh`
- Status: `302` (temporary redirect; move to `308` only with a deliberate
  stable decision)

## Enablement order

Enable the rule **only after the release asset exists**: the target URL

https://github.com/kubedoio/o3k-rust/releases/download/v0.2.0-alpha.2/install.sh

must return HTTP 200 before the redirect rule is turned on. Never point
get.o3k.io at an unpublished asset.

## Verification

After enabling the rule:

```bash
curl -sI https://get.o3k.io
# expect: HTTP/2 302 with location:
# https://github.com/kubedoio/o3k-rust/releases/download/v0.2.0-alpha.2/install.sh

curl -sfL https://get.o3k.io | sha256sum
# must equal the SHA256 of the published install.sh asset (recorded in the
# bundle manifest.json installer_sha256 of the v0.2.0-alpha.2 release)
```

The direct GitHub URL must behave identically:

```bash
curl -sfL \
  https://github.com/kubedoio/o3k-rust/releases/download/v0.2.0-alpha.2/install.sh \
  | sha256sum
```

This proves Cloudflare is merely convenience and not part of O3K's trusted
installation implementation.
