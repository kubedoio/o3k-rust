#!/usr/bin/env bash
set -Eeuo pipefail

OUTPUT_DIR=/etc/o3k/tls
SERVER_NAME=o3k-control-plane
FORCE=0
while (($#)); do
  case "$1" in
    --output-dir) OUTPUT_DIR="${2:?missing output directory}"; shift 2;;
    --server-name) SERVER_NAME="${2:?missing server name}"; shift 2;;
    --force) FORCE=1; shift;;
    *) echo "unknown option: $1" >&2; exit 2;;
  esac
done
[[ "$OUTPUT_DIR" == /* && "$OUTPUT_DIR" != / ]] || { echo "output directory must be an absolute non-root path" >&2; exit 2; }
command -v openssl >/dev/null 2>&1 || { echo "openssl is required" >&2; exit 1; }
[[ $FORCE -eq 1 || ! -e "$OUTPUT_DIR/ca.pem" ]] || { echo "certificates already exist; use --force to replace" >&2; exit 2; }
install -d -m 0700 "$OUTPUT_DIR"
TMP_DIR="$(mktemp -d "$OUTPUT_DIR/.tmp.XXXXXX")"
trap 'rmdir "$TMP_DIR" 2>/dev/null || true' EXIT
umask 077
openssl genpkey -algorithm ED25519 -out "$TMP_DIR/ca-key.pem" >/dev/null 2>&1
openssl req -x509 -new -key "$TMP_DIR/ca-key.pem" -out "$TMP_DIR/ca.pem" -days 365 -subj "/CN=O3K TestLab CA" >/dev/null 2>&1
openssl genpkey -algorithm ED25519 -out "$TMP_DIR/agent-key.pem" >/dev/null 2>&1
openssl req -new -key "$TMP_DIR/agent-key.pem" -out "$TMP_DIR/agent.csr" -subj "/CN=o3k-compute-agent" >/dev/null 2>&1
openssl x509 -req -in "$TMP_DIR/agent.csr" -CA "$TMP_DIR/ca.pem" -CAkey "$TMP_DIR/ca-key.pem" -CAcreateserial -out "$TMP_DIR/agent.pem" -days 365 >/dev/null 2>&1
for file in ca.pem agent.pem agent-key.pem; do install -m 0600 "$TMP_DIR/$file" "$OUTPUT_DIR/$file"; done
rm -f -- "$OUTPUT_DIR/ca-key.pem" "$OUTPUT_DIR/agent.csr" "$OUTPUT_DIR/ca.srl"
echo "generated O3K TestLab certificates under $OUTPUT_DIR for $SERVER_NAME"
