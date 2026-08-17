#!/usr/bin/env bash
set -euo pipefail

echo "==> Running Helm lint on o3k chart..."
helm lint deployments/helm/o3k/

echo "==> Testing default Helm template..."
helm template test-release deployments/helm/o3k/ > /dev/null

echo "==> Testing negative validation: replicaCount > 1 must be rejected..."
if helm template test-release deployments/helm/o3k/ --set replicaCount=2 2>/dev/null; then
    echo "ERROR: helm template accepted replicaCount=2, but single-controller invariant requires replicaCount=1" >&2
    exit 1
fi
echo "    PASS: replicaCount > 1 correctly rejected"

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
