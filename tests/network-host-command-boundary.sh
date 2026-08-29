#!/usr/bin/env bash
set -euo pipefail

network_root="crates/o3k-network/src"

production_section() {
    sed -n '1,/^#[[]cfg(test)[]]/p' "$1"
}

for module in lib gateway canonical_policy public policy routed; do
    if production_section "$network_root/$module.rs" | rg -n 'Command::new|std::process::Command|tokio::process::Command'; then
        echo "network host-command boundary: direct production command in $module.rs" >&2
        exit 1
    fi
done

for adapter in gateway_execution policy_execution public_execution routed_execution; do
    rg -q 'Command::new' "$network_root/linux_fabric/$adapter.rs" || {
        echo "network host-command boundary: missing command adapter in $adapter.rs" >&2
        exit 1
    }
done

if rg -n '(^|[[:space:]])(sh|bash)[[:space:]]+-c([[:space:]]|$)' "$network_root"; then
    echo "network host-command boundary: shell execution found" >&2
    exit 1
fi

echo "network host-command boundary: PASS"
