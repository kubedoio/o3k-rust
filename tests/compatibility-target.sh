#!/usr/bin/env bash
set -euo pipefail

python3 - <<'PY'
import json
import re
import subprocess
from pathlib import Path

root = Path.cwd()
target = json.loads((root / "docs/compatibility/target.json").read_text())
baseline = json.loads((root / "docs/specs/testlab-api-baseline.json").read_text())
manifest = (root / "compatibility/openstack-targets.yaml").read_text()
ci = (root / ".github/workflows/ci.yml").read_text()

assert target["decision"] == "static"
assert target["rust"]["toolchain"] == "1.97.1"
assert target["rust"]["cargo_rust_version"] == "1.97.1"
openstack = target["openstack"]
assert openstack["primary_profile"] == "gazpacho-2026.1"
assert openstack["backward_compatibility_profiles"] == ["flamingo-2025.2"]
assert openstack["forbidden_labels"] == ["2026.1 Flamingo"]
assert baseline["openstack_compatibility"] == {
    "series": "2026.1",
    "codename": "Gazpacho",
    "selection": "static",
    "specification": "compatibility/openstack-targets.yaml",
    "backward_compatibility": {"series": "2025.2", "codename": "Flamingo"},
}
assert "id: gazpacho-2026.1" in manifest
assert "release_series: \"2026.1\"" in manifest
assert "codename: Gazpacho" in manifest
assert "id: flamingo-2025.2" in manifest
assert "release_series: \"2025.2\"" in manifest
assert "codename: Flamingo" in manifest
assert "max_advertised_microversion: \"2.103\"" in manifest
assert "max_advertised_microversion: \"2.100\"" in manifest
assert "implemented_microversion_window: \"2.1-2.1\"" in manifest
assert "implemented_microversion_window: \"1.28-1.28\"" in manifest
assert "2026.1 Flamingo" not in (root / "README.md").read_text()
for path in (root / "Cargo.toml", root / "rust-toolchain.toml", root / ".github/workflows/ci.yml"):
    assert "1.85" not in path.read_text(), path
assert 'rust-version = "1.97.1"' in (root / "Cargo.toml").read_text()
assert 'channel = "1.97.1"' in (root / "rust-toolchain.toml").read_text()
assert "reads rust-toolchain.toml" in ci
assert not re.search(r"^\s+toolchain:\s*", ci, re.MULTILINE)
version = subprocess.run(["rustc", "--version"], check=True, capture_output=True, text=True).stdout.strip()
assert version.startswith("rustc 1.97.1 "), version
adr = (root / "docs/adr/ADR-0153-static-rust-and-openstack-release-policy.md").read_text()
spec = (root / "docs/specs/SPEC-0016-static-compatibility-target.md").read_text()
assert "#332" in adr and "2026.1 Gazpacho" in adr and "2025.2 Flamingo" in adr
assert "must not select a different profile" in adr
assert "2026.1" in spec and "Gazpacho" in spec and "Flamingo" in spec
assert "1.97.1" in spec
for source in target["sources"]:
    assert re.match(r"https://(docs\.openstack\.org|releases\.openstack\.org|blog\.rust-lang\.org)/", source), source
print("validated static Rust 1.97.1, Gazpacho primary, and Flamingo compatibility profile")
PY
