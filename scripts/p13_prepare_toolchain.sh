#!/usr/bin/env bash
set -euo pipefail

# Materialize the P13 provider toolchain from the pinned public manifest. This
# script is test/CI-only; artifacts are always kept outside the repository.
root_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
manifest="$root_dir/docs/compatibility/p13-1/provider-toolchain.json"

if [[ "${1:-}" == "--self-test" ]]; then
  self_test_dir=$(mktemp -d)
  trap 'rm -rf -- "$self_test_dir"' EXIT
  printf 'fixture\n' >"$self_test_dir/asset"
  expected=$(sha256sum "$self_test_dir/asset" | awk '{print $1}')
  check_hash() { [[ "$(sha256sum "$1" | awk '{print $1}')" == "$2" ]] || return 1; }
  check_hash "$self_test_dir/asset" "$expected"
  ! check_hash "$self_test_dir/asset" "${expected%?}0"
  ! check_hash "$self_test_dir/missing" "$expected" 2>/dev/null
  cp "$self_test_dir/asset" "$self_test_dir/cache"
  check_hash "$self_test_dir/cache" "$expected"
  printf 'corrupt\n' >"$self_test_dir/cache"
  ! check_hash "$self_test_dir/cache" "$expected"
  ! check_hash "$self_test_dir/asset" "$(printf 'wrong-open-tofu-archive' | sha256sum | awk '{print $1}')"
  ! check_hash "$self_test_dir/asset" "$(printf 'wrong-provider-archive' | sha256sum | awk '{print $1}')"
  platform_supported() { [[ "${O3K_P13_TEST_OS:-$(uname -s)}" == Linux && "${O3K_P13_TEST_ARCH:-$(uname -m)}" == x86_64 ]]; }
  ! O3K_P13_TEST_OS=Darwin O3K_P13_TEST_ARCH=arm64 platform_supported
  printf '%s\n' 'P13 toolchain bootstrap self-test: PASS'
  exit 0
fi

[[ "$(uname -s)" == Linux && "$(uname -m)" == x86_64 ]] || {
  echo 'P13 toolchain requires linux_amd64' >&2; exit 2;
}
command -v curl >/dev/null || { echo 'curl is required' >&2; exit 2; }
command -v sha256sum >/dev/null || { echo 'sha256sum is required' >&2; exit 2; }
command -v unzip >/dev/null || { echo 'unzip is required' >&2; exit 2; }

readarray -t pins < <(python3 - "$manifest" <<'PY'
import json, sys
m=json.load(open(sys.argv[1]))
for value in (m['engine']['version'], m['engine']['release_asset'], m['engine']['sha256'],
              m['provider']['version'], m['provider']['release_asset'],
              m['provider_archive_sha256'], m['provider_sha256']): print(value)
PY
)
[[ ${#pins[@]} -eq 7 ]] || { echo 'invalid P13 toolchain manifest' >&2; exit 2; }
engine_version=${pins[0]}; engine_asset=${pins[1]}; engine_hash=${pins[2]}
provider_version=${pins[3]}; provider_asset=${pins[4]}; provider_archive_hash=${pins[5]}; provider_hash=${pins[6]}
base_url="${O3K_P13_TOOLCHAIN_URL:-https://github.com/opentofu/opentofu/releases/download/v${engine_version}}"
provider_url="${O3K_P13_PROVIDER_URL:-https://github.com/terraform-provider-openstack/terraform-provider-openstack/releases/download/v${provider_version}}"
work_dir=${O3K_P13_TOOLCHAIN_DIR:-$(mktemp -d)}
mkdir -p "$work_dir"
tofu_archive="$work_dir/$engine_asset"
provider_archive="$work_dir/$provider_asset"
download() { local url=$1 out=$2 hash=$3; if [[ ! -f "$out" ]] || ! printf '%s  %s\n' "$hash" "$out" | sha256sum --check --status; then curl --fail --location --retry 4 --proto '=https' --tlsv1.2 --output "$out.tmp" "$url"; printf '%s  %s\n' "$hash" "$out.tmp" | sha256sum --check --status; mv -f -- "$out.tmp" "$out"; fi; printf '%s  %s\n' "$hash" "$out" | sha256sum --check --status; }
download "$base_url/$engine_asset" "$tofu_archive" "$engine_hash"
download "$provider_url/$provider_asset" "$provider_archive" "$provider_archive_hash"
extract_dir="$work_dir/extracted"; rm -rf -- "$extract_dir"; mkdir -p "$extract_dir/tofu" "$extract_dir/provider"
tar -xzf "$tofu_archive" -C "$extract_dir/tofu"
unzip -q "$provider_archive" -d "$extract_dir/provider"
tofu="$extract_dir/tofu/tofu"
provider=$(find "$extract_dir/provider" -type f -name 'terraform-provider-openstack*' -print -quit)
[[ -x "$tofu" && -n "$provider" ]] || { echo 'pinned toolchain extraction missing binary' >&2; exit 1; }
[[ "$("$tofu" version | head -n1)" == "OpenTofu v${engine_version}"* ]] || { echo 'OpenTofu version mismatch' >&2; exit 1; }
[[ "$provider" == *"${provider_version}"* ]] || { echo 'provider version mismatch' >&2; exit 1; }
printf '%s  %s\n' "$provider_hash" "$provider" | sha256sum --check --status || { echo 'provider binary checksum mismatch' >&2; exit 1; }
export O3K_P13_TOFU="$tofu" O3K_P13_TOFU_ARCHIVE="$tofu_archive" O3K_P13_PROVIDER_ARCHIVE="$provider_archive" O3K_P13_PROVIDER_BINARY="$provider" O3K_P13_PROVIDER_SHA256="$provider_hash"
python3 "$root_dir/scripts/p13_provider_contract.py" --verify-tools >/dev/null
printf 'export O3K_P13_TOFU=%q\nexport O3K_P13_TOFU_ARCHIVE=%q\nexport O3K_P13_PROVIDER_ARCHIVE=%q\nexport O3K_P13_PROVIDER_BINARY=%q\nexport O3K_P13_PROVIDER_SHA256=%q\n' "$O3K_P13_TOFU" "$O3K_P13_TOFU_ARCHIVE" "$O3K_P13_PROVIDER_ARCHIVE" "$O3K_P13_PROVIDER_BINARY" "$O3K_P13_PROVIDER_SHA256"
