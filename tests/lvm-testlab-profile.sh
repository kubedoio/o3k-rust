#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${ROOT_DIR}/scripts/lvm-testlab-profile.sh"

bash -n "${SCRIPT}"
python3 - "${SCRIPT}" <<'PY'
from pathlib import Path
import sys

text = Path(sys.argv[1]).read_text(encoding="utf-8")
assert "refusing to adopt pre-existing VG" in text
assert "refusing to remove an unmarked or foreign VG" in text
assert "o3k_storage_" in text and "o3k_pool_" in text
assert "rm -rf -- \"${STATE_ROOT}\"" in text
assert "losetup --find --show" in text
assert "provider_namespace" in text
PY

echo "LVM TestLab profile guard checks passed"
