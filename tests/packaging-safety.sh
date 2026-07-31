#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-packaging.XXXXXX")"
trap 'rm -rf -- "${WORK_DIR}"' EXIT
BINARY="${O3K_PACKAGING_BINARY:-${ROOT_DIR}/target/debug/o3kd}"
if [[ ! -x "$BINARY" ]]; then
  cargo build --manifest-path "$ROOT_DIR/Cargo.toml" --bin o3kd >/dev/null
fi
[[ -x "$BINARY" ]] || { echo "packaging test binary is missing: $BINARY" >&2; exit 2; }

mkdir -p "$WORK_DIR/preexisting"
printf 'keep me\n' >"$WORK_DIR/preexisting/user-data"
if bash "$ROOT_DIR/packaging/install.sh" \
    --profile fake --noninteractive --binary "$BINARY" \
    --prefix "$WORK_DIR/prefix" --data-dir "$WORK_DIR/preexisting" \
    --config-dir "$WORK_DIR/config" --log-dir "$WORK_DIR/log"; then
  echo "installer claimed a populated unowned directory" >&2
  exit 1
fi
[[ -f "$WORK_DIR/preexisting/user-data" ]]

bash "$ROOT_DIR/packaging/install.sh" \
  --profile fake --noninteractive --binary "$BINARY" \
  --prefix "$WORK_DIR/prefix" --data-dir "$WORK_DIR/data" \
  --config-dir "$WORK_DIR/config" --log-dir "$WORK_DIR/log"
mkdir -p "$WORK_DIR/fake-bin"
cat >"$WORK_DIR/fake-bin/systemctl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$SYSTEMCTL_LOG"
EOF
chmod +x "$WORK_DIR/fake-bin/systemctl"
PATH="$WORK_DIR/fake-bin:$PATH" SYSTEMCTL_LOG="$WORK_DIR/systemctl.log" \
  bash "$ROOT_DIR/packaging/reset.sh" --yes --data-dir "$WORK_DIR/data" --log-dir "$WORK_DIR/log"
grep -Fqx 'stop o3k-compute.service' "$WORK_DIR/systemctl.log"
grep -Fqx 'stop o3kd.service' "$WORK_DIR/systemctl.log"
[[ -f "$WORK_DIR/data/.o3k-owned" && -f "$WORK_DIR/log/.o3k-owned" ]]

printf '%s\n' 'foreign helper' >"$WORK_DIR/prefix/share/o3k/foreign-helper.sh"
PATH="$WORK_DIR/fake-bin:$PATH" SYSTEMCTL_LOG="$WORK_DIR/systemctl.log" \
  bash "$WORK_DIR/prefix/share/o3k/uninstall.sh" \
  --prefix "$WORK_DIR/prefix" --data-dir "$WORK_DIR/data" \
  --config-dir "$WORK_DIR/config" --log-dir "$WORK_DIR/log"
for file in o3kd.service o3k-compute.service reset.sh uninstall.sh diagnose.sh preflight.sh bootstrap-certs.sh; do
  [[ ! -e "$WORK_DIR/prefix/share/o3k/$file" ]] || { echo "uninstall left helper file: $file" >&2; exit 1; }
done
[[ ! -e "$WORK_DIR/prefix/bin/o3kd" && ! -e "$WORK_DIR/prefix/bin/o3k-compute" ]]
[[ -f "$WORK_DIR/prefix/share/o3k/foreign-helper.sh" ]]
[[ -f "$WORK_DIR/data/.o3k-owned" && -f "$WORK_DIR/config/o3kd.env" && -f "$WORK_DIR/log/.o3k-owned" ]]

bash "$ROOT_DIR/packaging/uninstall.sh" --purge --yes \
  --prefix "$WORK_DIR/prefix" --data-dir "$WORK_DIR/data" \
  --config-dir "$WORK_DIR/config" --log-dir "$WORK_DIR/log"
[[ ! -e "$WORK_DIR/data" && ! -e "$WORK_DIR/config" && ! -e "$WORK_DIR/log" ]]

TLS_DIR="$WORK_DIR/certs-root/tls"
bash "$ROOT_DIR/packaging/bootstrap-certs.sh" \
  --output-dir "$TLS_DIR" --server-name o3k-control-plane --agent-id compute-agent
for file in ca.pem server.pem server-key.pem agent.pem agent-key.pem agent-id agent-fingerprint; do
  [[ -s "$TLS_DIR/$file" ]] || { echo "missing generated TLS file: $file" >&2; exit 1; }
done
openssl x509 -in "$TLS_DIR/server.pem" -noout -checkend 0
openssl x509 -in "$TLS_DIR/agent.pem" -noout -checkend 0
grep -q 'DNS:o3k-control-plane' <(openssl x509 -in "$TLS_DIR/server.pem" -noout -text)
grep -q 'URI:urn:o3k:compute:agent:compute-agent' <(openssl x509 -in "$TLS_DIR/agent.pem" -noout -text)
[[ "$(wc -c <"$TLS_DIR/agent-fingerprint")" -ge 64 ]]

grep -Fq '"O3K_PROVIDER=libvirt"' "$ROOT_DIR/packaging/install.sh"
echo "packaging safety tests passed"
