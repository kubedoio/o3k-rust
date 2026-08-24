#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "${root_dir}/scripts/p13_provider_contract.py" --validate
python3 "${root_dir}/scripts/p13_provider_contract.py" --self-test
bash -n "${root_dir}/tests/p13_1b_provider_harness.sh"

if [[ "${O3K_P13_RUN_REAL:-0}" == "1" ]]; then
  python3 "${root_dir}/scripts/p13_provider_contract.py" --verify-tools
  python3 "${root_dir}/scripts/p13_provider_contract.py" --run-real
else
  echo "P13.1A offline validation passed; set O3K_P13_RUN_REAL=1 for the real-provider gate"
fi
