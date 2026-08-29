#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
main="$repo_root/bins/o3k-compute/src/main.rs"
runtime="$repo_root/bins/o3k-compute/src/runtime.rs"
iscsi="$repo_root/bins/o3k-compute/src/iscsi.rs"
process="$repo_root/bins/o3k-compute/src/process.rs"

if rg -n 'Command::new|process::Command' "$main"; then
    echo "ERROR: compute composition root contains host command execution" >&2
    exit 1
fi

if rg -n 'Command::new|process::Command' "$runtime"; then
    echo "ERROR: ordinary compute runtime contains host command execution" >&2
    exit 1
fi

rg -n 'Command::new|process::Command' "$iscsi" >/dev/null
rg -n 'pidfd_open|pidfd_send_signal' "$process" >/dev/null

echo "compute host-command boundary: PASS"

