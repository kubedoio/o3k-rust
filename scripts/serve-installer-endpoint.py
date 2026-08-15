#!/usr/bin/env python3
"""Local byte-identical shim for the get.o3k.io installer endpoint (issue #613).

Implements the exact production Worker route contract from
packaging/get-o3k-worker/ (see docs/plan/one-line-installer.md):

    GET /                          -> packaging/get-o3k.sh verbatim
    GET /install.sh                -> packaging/get-o3k.sh verbatim
    GET /version                   -> the advertised version (plain text)
    GET /channel/alpha             -> v0.3.0-alpha.1 (plain text; Worker-route
                                      parity only — the installer itself is
                                      pinned and never consults a channel)
    GET /v<version>                -> O3K_PINNED_VERSION="<version>" first line
                                      (the BARE version, no leading "v" —
                                      byte-identical to the production Worker,
                                      packaging/get-o3k-worker/src/index.js)
                                      followed by the script; 400 unless the
                                      version matches the ADR-0130 fence
    GET /releases/v<version>/o3k-<version>-linux-x86_64.tar.gz(.sha256)
                                   -> the release assets (GitHub-Releases shape)

Anything else is 404. Stdlib only; no TLS — this shim is for the local
acceptance campaign where the guest reaches the host over slirp
(http://10.0.2.2:<port>). Production URLs in get-o3k.sh stay HTTPS; HTTP is
only permitted through the documented O3K_RELEASE_BASE override.

Usage:
    python3 scripts/serve-installer-endpoint.py \
        --port 18000 \
        --bundle-dist dist/o3k-0.3.0-alpha.1 \
        --version v0.3.0-alpha.1
The release assets are read from the parent of --bundle-dist (the dist dir
holding o3k-<version>-linux-x86_64.tar.gz + .sha256).
"""
import argparse
import http.server
import pathlib
import re
import sys

VERSION_RE = re.compile(r"^v?[0-9]+(\.[0-9]+){1,2}(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$")
ASSET_NAME = "o3k-{version}-linux-x86_64.tar.gz"
ALLOWED_ASSETS = {ASSET_NAME, ASSET_NAME + ".sha256"}


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=18000)
    parser.add_argument("--bundle-dist", required=True,
                        help="release bundle directory (dist/o3k-<version>/)")
    parser.add_argument("--version", default="v0.3.0-alpha.1")
    return parser.parse_args()


def main():
    args = parse_args()
    if not VERSION_RE.fullmatch(args.version):
        print(f"unsupported served version: {args.version}", file=sys.stderr)
        return 2
    version = args.version.lstrip("v")
    bundle_dir = pathlib.Path(args.bundle_dist).resolve()
    wrapper = bundle_dir / "packaging" / "get-o3k.sh"
    if not wrapper.is_file():
        print(f"wrapper script missing: {wrapper}", file=sys.stderr)
        return 2
    dist_dir = bundle_dir.parent
    assets = {name.format(version=version): dist_dir / name.format(version=version)
              for name in ALLOWED_ASSETS}
    missing = [str(path) for path in assets.values() if not path.is_file()]
    if missing:
        print("release assets missing: " + ", ".join(missing), file=sys.stderr)
        return 2
    wrapper_body = wrapper.read_bytes()
    # Byte-identical to the production Worker: the pin line carries the BARE
    # version (no leading "v") — packaging/get-o3k.sh accepts both shapes
    # (check_version_format strips an optional leading "v").
    pinned_body = (f'O3K_PINNED_VERSION="{version}"\n'.encode()
                   + wrapper_body)

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, fmt, *parts):  # keep logs; evidence directory
            sys.stderr.write("%s - %s\n" % (self.address_string(),
                                            fmt % parts))

        def send_bytes(self, body, content_type, status=200):
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            path = self.path.split("?", 1)[0]
            if path in ("/", "/install.sh"):
                self.send_bytes(wrapper_body, "text/plain; charset=utf-8")
                return
            if path == "/version":
                self.send_bytes(args.version.encode(), "text/plain")
                return
            if path == "/channel/alpha":
                self.send_bytes(args.version.encode(), "text/plain")
                return
            if path.startswith("/v"):
                pinned = path[2:]
                if VERSION_RE.fullmatch("v" + pinned):
                    self.send_bytes(pinned_body, "text/plain; charset=utf-8")
                    return
                self.send_bytes(b"unsupported version pin\n", "text/plain", 400)
                return
            prefix = "/releases/v%s/" % version
            if path.startswith(prefix):
                asset = path[len(prefix):]
                if asset in assets:
                    self.send_bytes(assets[asset].read_bytes(),
                                    "application/octet-stream")
                    return
            self.send_bytes(b"not found\n", "text/plain", 404)

    server = http.server.ThreadingHTTPServer(("0.0.0.0", args.port), Handler)
    print(f"serving get.o3k.io shim on 0.0.0.0:{args.port} "
          f"(version={args.version}, assets={dist_dir})", file=sys.stderr)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
