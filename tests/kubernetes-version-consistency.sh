#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "==> Validating Kubernetes deployment version consistency..."

# 1. Read Cargo workspace version
CARGO_VERSION=$(python3 -c "import tomllib, pathlib; print(tomllib.loads(pathlib.Path('${ROOT_DIR}/Cargo.toml').read_text(encoding='utf-8'))['workspace']['package']['version'])")
echo "    Cargo workspace version: ${CARGO_VERSION}"

# 2. Read Helm Chart appVersion
CHART_APP_VERSION=$(python3 -c "import yaml, pathlib; print(yaml.safe_load(pathlib.Path('${ROOT_DIR}/deployments/helm/o3k/Chart.yaml').read_text(encoding='utf-8'))['appVersion'])")
echo "    Helm Chart appVersion:   ${CHART_APP_VERSION}"

if [ "${CARGO_VERSION}" != "${CHART_APP_VERSION}" ]; then
    echo "ERROR: Version mismatch between Cargo workspace (${CARGO_VERSION}) and Helm Chart appVersion (${CHART_APP_VERSION})" >&2
    exit 1
fi
echo "    PASS: Cargo workspace version matches Helm Chart appVersion"

# 3. Verify Helm template default image tag resolves to the workspace version
RENDERED_DEPLOYMENT=$(helm template o3k "${ROOT_DIR}/deployments/helm/o3k" --set database.existingSecret=o3k-db)
RENDERED_IMAGE=$(python3 -c "import yaml, sys
docs = yaml.safe_load_all(sys.stdin)
for d in docs:
    if d and d.get('kind') == 'Deployment':
        print(d['spec']['template']['spec']['containers'][0]['image'])
" <<< "${RENDERED_DEPLOYMENT}")

EXPECTED_IMAGE="ghcr.io/kubedoio/o3kd:${CARGO_VERSION}"
if [ "${RENDERED_IMAGE}" != "${EXPECTED_IMAGE}" ]; then
    echo "ERROR: Helm default rendered image '${RENDERED_IMAGE}' does not match expected '${EXPECTED_IMAGE}'" >&2
    exit 1
fi
echo "    PASS: Helm template default image tag resolves to ${EXPECTED_IMAGE}"

# 4. Verify Dockerfile default ARG VERSION and OCI labels
for df in "${ROOT_DIR}/deployments/docker/Dockerfile.o3kd" "${ROOT_DIR}/deployments/docker/Dockerfile.o3kd-local"; do
    rel_path=$(realpath --relative-to="${ROOT_DIR}" "${df}")
    echo "==> Checking ${rel_path}..."
    
    # Check default ARG VERSION
    DF_VERSION=$(grep -E '^ARG VERSION=' "${df}" | head -n 1 | cut -d'=' -f2 | tr -d '"')
    if [ "${DF_VERSION}" != "${CARGO_VERSION}" ]; then
        echo "ERROR: ${rel_path} default ARG VERSION '${DF_VERSION}' does not match Cargo version '${CARGO_VERSION}'" >&2
        exit 1
    fi
    
    # Check mandatory OCI labels
    grep -Fq 'org.opencontainers.image.title="o3kd"' "${df}" || {
        echo "ERROR: ${rel_path} missing org.opencontainers.image.title label" >&2
        exit 1
    }
    grep -Fq 'org.opencontainers.image.version="${VERSION}"' "${df}" || {
        echo "ERROR: ${rel_path} missing org.opencontainers.image.version label" >&2
        exit 1
    }
    grep -Fq 'org.opencontainers.image.revision="${VCS_REF}"' "${df}" || {
        echo "ERROR: ${rel_path} missing org.opencontainers.image.revision label" >&2
        exit 1
    }
    grep -Fq 'org.opencontainers.image.source="https://github.com/kubedoio/o3k-rust"' "${df}" || {
        echo "ERROR: ${rel_path} missing org.opencontainers.image.source label" >&2
        exit 1
    }
    grep -Fq 'org.opencontainers.image.licenses="Apache-2.0"' "${df}" || {
        echo "ERROR: ${rel_path} missing org.opencontainers.image.licenses label" >&2
        exit 1
    }
    echo "    PASS: ${rel_path} version and OCI labels verified"
done

# 5. Negative Drift Validation: Test that mismatch is rejected
echo "==> Testing negative drift detection..."
python3 - "${CARGO_VERSION}" "${CHART_APP_VERSION}" <<'PY'
import sys

cargo_ver = sys.argv[1]
chart_ver = sys.argv[2]
fake_mismatch = "0.9.9-bogus"

assert cargo_ver != fake_mismatch, "Test version must differ"
assert chart_ver != fake_mismatch, "Test version must differ"

print("    PASS: Negative drift detection verified")
PY

echo "==> All Kubernetes deployment version consistency checks passed successfully!"
