#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ACTIONLINT="${1:-actionlint}"

if ! command -v "${ACTIONLINT}" >/dev/null 2>&1; then
    echo "workflow validator not found: ${ACTIONLINT}" >&2
    exit 127
fi

mapfile -d '' workflows < <(
    find "${ROOT_DIR}/.github/workflows" -maxdepth 1 -type f \
        \( -name '*.yml' -o -name '*.yaml' \) -print0 | sort -z
)

if ((${#workflows[@]} == 0)); then
    echo "no GitHub Actions workflows found" >&2
    exit 1
fi

for workflow in "${workflows[@]}"; do
    echo "Validating ${workflow#"${ROOT_DIR}/"}"
    "${ACTIONLINT}" "${workflow}"
done
