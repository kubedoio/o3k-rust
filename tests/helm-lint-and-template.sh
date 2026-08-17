#!/usr/bin/env bash
set -euo pipefail

echo "==> Running Helm lint on o3k chart..."
helm lint deployments/helm/o3k/

echo "==> Testing default Helm template..."
helm template test-release deployments/helm/o3k/ > /dev/null

echo "==> Testing negative validation: replicaCount > 1 without multiController.enabled must be rejected..."
if helm template test-release deployments/helm/o3k/ --set replicaCount=3 2>/dev/null; then
    echo "ERROR: helm template accepted replicaCount=3 without multiController.enabled=true" >&2
    exit 1
fi
echo "    PASS: replicaCount > 1 without multiController.enabled correctly rejected"

echo "==> Testing multi-controller validation: replicaCount=3 with multiController.enabled=true..."
OUT_3=$(helm template test-release deployments/helm/o3k/ --set replicaCount=3 --set multiController.enabled=true)
echo "$OUT_3" | grep -q "replicas: 3"
echo "$OUT_3" | grep -q "type: RollingUpdate"
echo "    PASS: replicaCount=3 rendered with RollingUpdate strategy"

echo "==> Testing negative validation: replicaCount=0 must be rejected..."
if helm template test-release deployments/helm/o3k/ --set replicaCount=0 2>/dev/null; then
    echo "ERROR: helm template accepted replicaCount=0, but minimum is 1" >&2
    exit 1
fi
echo "    PASS: replicaCount=0 correctly rejected"

echo "==> Testing negative validation: database.backend=sqlite must be rejected..."
if helm template test-release deployments/helm/o3k/ --set database.backend=sqlite 2>/dev/null; then
    echo "ERROR: helm template accepted database.backend=sqlite, but Kubernetes profile strictly requires postgres" >&2
    exit 1
fi
echo "    PASS: database.backend=sqlite correctly rejected"

echo "==> Testing Helm template with TLS and auth Secret..."
OUT=$(helm template test-release deployments/helm/o3k/ \
    --set tls.enabled=true \
    --set tls.authorizedAgents="agent-1=abc123" \
    --set auth.existingSecret="custom-auth-secret")

echo "$OUT" | grep -q "O3K_COMPUTE_SERVER_CERTIFICATE"
echo "$OUT" | grep -q "O3K_COMPUTE_AUTHORIZED_AGENTS"
echo "$OUT" | grep -q "custom-auth-secret"
echo "    PASS: TLS and auth secret configurations rendered correctly"

echo "==> All Helm lint, template, and schema validation tests passed!"
