#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
helper="$repo_root/scripts/ci/apt-provision.sh"
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
mkdir -p "$root/etc/apt/sources.list.d"

cat > "$root/etc/apt/sources.list" <<'EOF'
deb http://azure.archive.ubuntu.com/ubuntu noble main
EOF
cat > "$root/etc/apt/sources.list.d/google-chrome.list" <<'EOF'
deb [arch=amd64] https://dl.google.com/linux/chrome/deb stable main
EOF
cat > "$root/etc/apt/sources.list.d/vendor.sources" <<'EOF'
Types: deb
URIs: https://packages.example.invalid/ubuntu
Suites: noble
Components: main
EOF
cat > "$root/etc/apt/sources.list.d/ubuntu.sources" <<'EOF'
Types: deb
URIs: http://archive.ubuntu.com/ubuntu
Suites: noble
Components: main
EOF

rendered="$root/rendered.list"
O3K_CI_APT_ROOT="$root" "$helper" render "$rendered"
grep -q 'archive.ubuntu.com/ubuntu noble main' "$rendered/sources.list"
grep -q 'URIs: http://archive.ubuntu.com/ubuntu' "$rendered/parts/ubuntu.sources"
! grep -R -q 'dl.google.com' "$rendered"
! grep -R -q 'packages.example.invalid' "$rendered"

required="$root/required.list"
O3K_CI_APT_ROOT="$root" O3K_CI_APT_REQUIRED_DOMAINS=packages.example.invalid \
  "$helper" render "$required"
grep -R -q 'packages.example.invalid' "$required"

echo 'APT source hygiene tests: PASS'
