#!/usr/bin/env bash
set -euo pipefail

# Usage: eval "$(scripts/p13_2_provider_tools.sh /absolute/path/to/toolchain)"
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out_dir="${1:?usage: $0 /absolute/output-directory}"
manifest="$root_dir/docs/compatibility/p13-1/provider-toolchain.json"
mkdir -p "$out_dir" "$out_dir/tofu" "$out_dir/provider"
curl -fL --retry 3 -o "$out_dir/tofu_1.12.6_linux_amd64.tar.gz" "https://github.com/opentofu/opentofu/releases/download/v1.12.6/tofu_1.12.6_linux_amd64.tar.gz"
curl -fL --retry 3 -o "$out_dir/terraform-provider-openstack_3.4.0_linux_amd64.zip" "https://github.com/terraform-provider-openstack/terraform-provider-openstack/releases/download/v3.4.0/terraform-provider-openstack_3.4.0_linux_amd64.zip"
python3 - "$manifest" "$out_dir" <<'PY'
import hashlib, json, pathlib, sys
manifest = json.loads(pathlib.Path(sys.argv[1]).read_text())
root = pathlib.Path(sys.argv[2])
def sha(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""): h.update(chunk)
    return h.hexdigest()
checks = {
    root / "tofu_1.12.6_linux_amd64.tar.gz": manifest["engine"]["sha256"],
    root / "terraform-provider-openstack_3.4.0_linux_amd64.zip": manifest["provider_archive_sha256"],
}
for path, expected in checks.items():
    actual = sha(path)
    if actual != expected: raise SystemExit(f"checksum mismatch for {path}: {actual}")
print(f"export O3K_P13_TOFU_ARCHIVE={root / 'tofu_1.12.6_linux_amd64.tar.gz'}")
print(f"export O3K_P13_PROVIDER_ARCHIVE={root / 'terraform-provider-openstack_3.4.0_linux_amd64.zip'}")
print(f"export O3K_P13_PROVIDER_SHA256={manifest['provider_sha256']}")
PY
tar -xzf "$out_dir/tofu_1.12.6_linux_amd64.tar.gz" -C "$out_dir/tofu"
unzip -oq "$out_dir/terraform-provider-openstack_3.4.0_linux_amd64.zip" -d "$out_dir/provider"
chmod 0755 "$out_dir/tofu/tofu" "$out_dir/provider/terraform-provider-openstack_v3.4.0"
printf 'export O3K_P13_TOFU=%q\n' "$out_dir/tofu/tofu"
printf 'export O3K_P13_PROVIDER_BINARY=%q\n' "$out_dir/provider/terraform-provider-openstack_v3.4.0"
