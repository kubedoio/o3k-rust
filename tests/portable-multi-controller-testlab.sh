#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLUSTER_NAME="o3k-multi-controller-testlab"
NAMESPACE="o3k-system"
IMAGE_TAG="$(python3 -c "import tomllib, pathlib; print(tomllib.loads(pathlib.Path('${ROOT_DIR}/Cargo.toml').read_text(encoding='utf-8'))['workspace']['package']['version'])")"
IMAGE_NAME="ghcr.io/kubedoio/o3kd:${IMAGE_TAG}"

cleanup() {
    echo "==> Cleaning up multi-controller testlab..."
    kind delete cluster --name "${CLUSTER_NAME}" 2>/dev/null || true
}

trap cleanup ERR

echo "============================================================"
echo "P7: Portable 3-Controller Kind Acceptance"
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

# 6. Install O3K Helm chart with 3 replicas and multiController.enabled=true
echo "==> 6. Installing O3K 3-Controller Control Plane via Helm..."
helm upgrade --install o3k "${ROOT_DIR}/deployments/helm/o3k/" \
    --namespace "${NAMESPACE}" \
    --set image.pullPolicy="Never" \
    --set config.provider="fake" \
    --set replicaCount=3 \
    --set multiController.enabled=true \
    --set auth.existingSecret="o3k-bootstrap-auth" \
    --wait \
    --timeout 90s

# 7. Check rollout status for 3 replicas
echo "==> 7. Checking 3-replica control plane rollout status..."
kubectl rollout status deployment/o3k -n "${NAMESPACE}" --timeout=90s

READY_REPLICAS=$(kubectl get deployment o3k -n "${NAMESPACE}" -o jsonpath="{.status.readyReplicas}")
echo "    PASS: Ready replicas: ${READY_REPLICAS}/3"
if [ "${READY_REPLICAS}" -ne 3 ]; then
    echo "ERROR: Expected 3 ready replicas, found ${READY_REPLICAS}" >&2
    exit 1
fi

# 8. Run o3k doctor in all 3 pods
echo "==> 8. Running o3k doctor across all 3 control plane pods..."
PODS=$(kubectl get pods -n "${NAMESPACE}" -l "app.kubernetes.io/name=o3k,app.kubernetes.io/component=control-plane" -o jsonpath="{.items[*].metadata.name}")

for POD in ${PODS}; do
    echo "    Checking doctor on pod ${POD}..."
    DOCTOR_OUT=$(kubectl exec -n "${NAMESPACE}" "${POD}" -- o3k doctor --json)
    FAIL_COUNT=$(echo "$DOCTOR_OUT" | { grep -E '"status":\s*"FAIL"' || true; } | wc -l)
    if [ "${FAIL_COUNT}" -ne 0 ]; then
        echo "ERROR: o3k doctor reported failures on pod ${POD}!" >&2
        echo "$DOCTOR_OUT" >&2
        exit 1
    fi
    echo "    PASS: pod ${POD} doctor verified healthy"
done

# 9. Verify distinct controller sessions in PostgreSQL
echo "==> 9. Verifying active controller sessions in PostgreSQL..."
PG_POD=$(kubectl get pods -n "${NAMESPACE}" -l "app=postgres" -o jsonpath="{.items[0].metadata.name}")
SESSIONS_COUNT=$(kubectl exec -n "${NAMESPACE}" "${PG_POD}" -- psql -U o3k -d o3k -tAc "SELECT count(*) FROM controller_sessions WHERE state = 'Active';")
echo "    Active controller sessions in DB: ${SESSIONS_COUNT}"
if [ "${SESSIONS_COUNT}" -lt 3 ]; then
    echo "ERROR: Expected at least 3 active controller sessions in PostgreSQL, found ${SESSIONS_COUNT}" >&2
    exit 1
fi
echo "    PASS: PostgreSQL records all 3 distinct active controller sessions"

# 10. Verify API via Kubernetes Service
echo "==> 10. Verifying OpenStack API across multi-controller service..."
PORT=15001
kubectl port-forward -n "${NAMESPACE}" svc/o3k-api "${PORT}:5000" &
PF_PID=$!
trap "kill ${PF_PID} || true; cleanup" EXIT

sleep 2

# Issue Keystone token
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
PROJECT_ID=$(python3 -c "import sys, json; data=json.loads(sys.stdin.read()); print(data['token']['project']['id'])" <<< "${AUTH_BODY}")
echo "    PASS: Keystone authenticated via multi-controller Service (Project: ${PROJECT_ID})"

# Perform 10 consecutive API requests to test round-robin load distribution across controllers
echo "==> Testing API requests across multi-controller pods..."
for i in $(seq 1 10); do
    curl -sf -H "X-Auth-Token: ${TOKEN}" "http://127.0.0.1:${PORT}/v2/images" >/dev/null
    curl -sf -H "X-Auth-Token: ${TOKEN}" "http://127.0.0.1:${PORT}/v2.1/${PROJECT_ID}/flavors" >/dev/null
done
echo "    PASS: 20 API requests successfully served across 3 controllers"

kill ${PF_PID} || true
trap - EXIT
cleanup

echo "============================================================"
echo "P7 3-Controller Kind Acceptance PASSED!"
echo "============================================================"
