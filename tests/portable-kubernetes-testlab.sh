#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLUSTER_NAME="o3k-testlab"
NAMESPACE="o3k-system"
IMAGE_TAG="$(python3 -c "import tomllib, pathlib; print(tomllib.loads(pathlib.Path('${ROOT_DIR}/Cargo.toml').read_text(encoding='utf-8'))['workspace']['package']['version'])")"
IMAGE_NAME="ghcr.io/kubedoio/o3kd:${IMAGE_TAG}"

cleanup() {
    echo "==> Cleaning up testlab..."
    kind delete cluster --name "${CLUSTER_NAME}" || true
}

# Trap cleanup on error
trap cleanup ERR

echo "============================================================"
echo "P6: Portable Kubernetes TestLab Acceptance"
echo "============================================================"

# 1. Create kind cluster
echo "==> 1. Creating kind cluster ${CLUSTER_NAME}..."
if kind get clusters 2>/dev/null | grep -q "^${CLUSTER_NAME}$"; then
    echo "    Reusing existing kind cluster..."
else
    kind create cluster --name "${CLUSTER_NAME}" --wait 60s
fi

# 2. Build and load o3kd container image into kind
echo "==> 2. Building and loading image ${IMAGE_NAME} into kind cluster..."
cargo build --release --bin o3kd --bin o3k
docker build -t "${IMAGE_NAME}" -f deployments/docker/Dockerfile.o3kd-local .
kind load docker-image "${IMAGE_NAME}" --name "${CLUSTER_NAME}"

# 3. Create namespace
echo "==> 3. Creating namespace ${NAMESPACE}..."
kubectl create namespace "${NAMESPACE}" --dry-run=client -o yaml | kubectl apply -f -

# 4. Deploy PostgreSQL 16 fixture
echo "==> 4. Deploying PostgreSQL 16 fixture in ${NAMESPACE}..."
cat <<EOF | kubectl apply -n "${NAMESPACE}" -f -
apiVersion: apps/v1
kind: Deployment
metadata:
  name: postgres
  labels:
    app: postgres
spec:
  replicas: 1
  selector:
    matchLabels:
      app: postgres
  template:
    metadata:
      labels:
        app: postgres
    spec:
      containers:
        - name: postgres
          image: postgres:16-alpine
          env:
            - name: POSTGRES_DB
              value: o3k
            - name: POSTGRES_USER
              value: o3k
            - name: POSTGRES_PASSWORD
              value: testlabsecret
          ports:
            - containerPort: 5432
          readinessProbe:
            exec:
              command: ["pg_isready", "-U", "o3k", "-d", "o3k"]
            initialDelaySeconds: 2
            periodSeconds: 2
---
apiVersion: v1
kind: Service
metadata:
  name: postgres
  labels:
    app: postgres
spec:
  ports:
    - port: 5432
      targetPort: 5432
  selector:
    app: postgres
EOF

echo "==> Waiting for PostgreSQL 16 to be ready..."
kubectl rollout status deployment/postgres -n "${NAMESPACE}" --timeout=60s

# 5. Create database and auth secrets
echo "==> 5. Creating database and auth Secrets..."
kubectl create secret generic o3k-database \
    --namespace "${NAMESPACE}" \
    --from-literal=database-url="postgres://o3k:testlabsecret@postgres.${NAMESPACE}.svc.cluster.local:5432/o3k" \
    --dry-run=client -o yaml | kubectl apply -f -

kubectl create secret generic o3k-bootstrap-auth \
    --namespace "${NAMESPACE}" \
    --from-literal=bootstrap-password="admin-testlab-password" \
    --from-literal=token-signing-key="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" \
    --dry-run=client -o yaml | kubectl apply -f -

# 6. Install O3K Helm chart
echo "==> 6. Installing O3K Control Plane via Helm..."
helm upgrade --install o3k "${ROOT_DIR}/deployments/helm/o3k/" \
    --namespace "${NAMESPACE}" \
    --set image.pullPolicy="Never" \
    --set config.provider="fake" \
    --set persistence.size="1Gi" \
    --set auth.existingSecret="o3k-bootstrap-auth" \
    --wait \
    --timeout 60s

# 7. Check rollout status
echo "==> 7. Checking control plane rollout status..."
kubectl rollout status deployment/o3k -n "${NAMESPACE}" --timeout=60s

# 8. Run o3k doctor inside the pod
echo "==> 8. Running o3k doctor in Kubernetes control plane pod..."
POD_NAME=$(kubectl get pods -n "${NAMESPACE}" -l "app.kubernetes.io/name=o3k,app.kubernetes.io/component=control-plane" -o jsonpath="{.items[0].metadata.name}")

DOCTOR_OUT=$(kubectl exec -n "${NAMESPACE}" "${POD_NAME}" -- o3k doctor --json)
echo "$DOCTOR_OUT"

# Verify doctor JSON output
PASS_COUNT=$(echo "$DOCTOR_OUT" | { grep -E '"status":\s*"PASS"' || true; } | wc -l)
FAIL_COUNT=$(echo "$DOCTOR_OUT" | { grep -E '"status":\s*"FAIL"' || true; } | wc -l)
NA_COUNT=$(echo "$DOCTOR_OUT" | { grep -E '"status":\s*"NOT_APPLICABLE"' || true; } | wc -l)

echo "Doctor Summary: PASS=${PASS_COUNT}, FAIL=${FAIL_COUNT}, NOT_APPLICABLE=${NA_COUNT}"

if [ "${FAIL_COUNT}" -ne 0 ]; then
    echo "ERROR: o3k doctor reported failures in Kubernetes control plane container!" >&2
    exit 1
fi

if [ "${PASS_COUNT}" -eq 0 ]; then
    echo "ERROR: o3k doctor did not report any passes!" >&2
    exit 1
fi
echo "    PASS: o3k doctor verified all control plane components healthy"

# 9. Verify API via Port-forward
echo "==> 9. Verifying OpenStack API endpoints..."
PORT=15000
kubectl port-forward -n "${NAMESPACE}" svc/o3k-api "${PORT}:5000" &
PF_PID=$!
trap "kill ${PF_PID} || true; cleanup" EXIT

sleep 2

# Check /healthz and /readyz
curl -sf "http://127.0.0.1:${PORT}/healthz" | grep -q "ok"
curl -sf "http://127.0.0.1:${PORT}/readyz" | grep -q "ready"
echo "    PASS: /healthz and /readyz endpoints healthy"

# Keystone token issuance
AUTH_RESP=$(curl -sf -i -X POST "http://127.0.0.1:${PORT}/v3/auth/tokens" \
    -H "Content-Type: application/json" \
    -d '{
        "auth": {
            "identity": {
                "methods": ["password"],
                "password": {
                    "user": {
                        "name": "admin",
                        "domain": {"name": "Default"},
                        "password": "admin-testlab-password"
                    }
                }
            },
            "scope": {
                "project": {
                    "name": "admin",
                    "domain": {"name": "Default"}
                }
            }
        }
    }')

AUTH_BODY=$(echo "${AUTH_RESP}" | sed -e '1,/^\r\{0,1\}$/d')
TOKEN=$(echo "${AUTH_RESP}" | grep -i "^x-subject-token:" | awk '{print $2}' | tr -d '\r\n')
if [ -z "${TOKEN}" ]; then
    echo "ERROR: failed to obtain Keystone token" >&2
    exit 1
fi
echo "    PASS: Keystone token issued: ${TOKEN:0:8}..."

PROJECT_ID=$(python3 -c "import sys, json; data=json.loads(sys.stdin.read()); print(data['token']['project']['id'])" <<< "${AUTH_BODY}")
echo "    PASS: Authenticated project ID: ${PROJECT_ID}"

# List images
curl -sf -H "X-Auth-Token: ${TOKEN}" "http://127.0.0.1:${PORT}/v2/images" | grep -q "images"
echo "    PASS: Glance image listing operational"

# List flavors
curl -sf -H "X-Auth-Token: ${TOKEN}" "http://127.0.0.1:${PORT}/v2.1/${PROJECT_ID}/flavors" | grep -q "flavors"
echo "    PASS: Nova flavor listing operational"

# List networks
curl -sf -H "X-Auth-Token: ${TOKEN}" "http://127.0.0.1:${PORT}/v2.0/networks" | grep -q "networks"
echo "    PASS: Neutron network listing operational"

# Placement discovery
curl -sf "http://127.0.0.1:${PORT}/placement" | grep -q "versions"
echo "    PASS: Placement discovery API operational"

# Clean up port-forward
kill ${PF_PID} || true
trap - EXIT

# Clean up kind cluster
cleanup

echo "============================================================"
echo "P6 Portable Kubernetes TestLab PASSED!"
echo "============================================================"
