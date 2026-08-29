#!/usr/bin/env bash
set -euo pipefail

network_root="crates/o3k-network/src"

has_forbidden_execution() {
    rg -n \
        'Command::new|std::process::Command|tokio::process::Command|run\([[:space:]]*"(ip|nft)"|output\([[:space:]]*"(ip|nft)"|GatewayCommand|PublicCommand|PolicyCommand|RoutedCommand|NetworkCommand' \
        "$1"
}

# Scan complete canonical files. #[cfg(test)] does not end the production
# section because production items may legally follow test-gated items.
for module in gateway canonical_policy public policy routed; do
    if has_forbidden_execution "$network_root/$module.rs"; then
        echo "network host-command boundary: direct execution or low-level command dependency in $module.rs" >&2
        exit 1
    fi
done

for adapter in gateway_execution policy_execution public_execution routed_execution network_execution; do
    rg -q 'Command::new' "$network_root/linux_fabric/$adapter.rs" || {
        echo "network host-command boundary: missing command adapter in $adapter.rs" >&2
        exit 1
    }
done

if rg -n '(^|[[:space:]])(sh|bash)[[:space:]]+-c([[:space:]]|$)' "$network_root"; then
    echo "network host-command boundary: shell execution found" >&2
    exit 1
fi

# Regression fixture: a command after an earlier cfg(test) item must still be
# rejected by the complete-file scan.
fixture="$(mktemp)"
printf '%s\n' '#[cfg(test)] mod tests {}' 'fn production_after_tests() { let _ = Command::new("ip"); }' > "$fixture"
if ! has_forbidden_execution "$fixture" >/dev/null; then
    echo "network host-command boundary: cfg(test) escape regression" >&2
    exit 1
fi

echo "network host-command boundary: PASS"
