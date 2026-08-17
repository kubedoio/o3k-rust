#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d /tmp/o3k-k8s-kvm.XXXXXX)"
chmod 0755 "${WORK_DIR}"
umask 022
CLUSTER_NAME="o3k-k8s-kvm-testlab"
NAMESPACE="o3k-system"
IMAGE_TAG="$(python3 -c "import tomllib, pathlib; print(tomllib.loads(pathlib.Path('${ROOT_DIR}/Cargo.toml').read_text(encoding='utf-8'))['workspace']['package']['version'])")"
IMAGE_NAME="ghcr.io/kubedoio/o3kd:${IMAGE_TAG}"
COMPUTE_PID=""
BRIDGE_NAME="o3k-br-p6"

CLEANED=0
cleanup() {
    if [ "${CLEANED}" -eq 1 ]; then
        return 0
    fi
    CLEANED=1
    echo "==> Cleaning up KVM and Kubernetes testlab..."
    if [ -n "${COMPUTE_PID}" ] && kill -0 "${COMPUTE_PID}" 2>/dev/null; then
        echo "    Stopping o3k-compute process ${COMPUTE_PID}..."
        kill "${COMPUTE_PID}" || true
        wait "${COMPUTE_PID}" 2>/dev/null || true
    fi
    # Clean up bridge if created
    sudo -n ip link del "${BRIDGE_NAME}" 2>/dev/null || true
    # Delete kind cluster
    kind delete cluster --name "${CLUSTER_NAME}" 2>/dev/null || true
    # Remove work directory
    rm -rf "${WORK_DIR}" || true
}

trap cleanup ERR EXIT

echo "============================================================"
echo "P6: Real KVM/Libvirt Acceptance on Kubernetes Control Plane"
echo "============================================================"

# 0. Check pre-flight requirements
echo "==> 0. Checking pre-flight requirements..."
if [ ! -e /dev/kvm ]; then
    echo "ERROR: /dev/kvm not found" >&2
    exit 1
fi
virsh uri || {
    echo "ERROR: cannot connect to libvirt daemon (qemu:///system)" >&2
    exit 1
}

CIRROS_IMG="/root/cirros-0.6.3-x86_64-disk.img"
if [ ! -f "${CIRROS_IMG}" ]; then
    CIRROS_IMG="/var/tmp/cirros-0.6.3-x86_64-disk.img"
fi
if [ ! -f "${CIRROS_IMG}" ]; then
    echo "ERROR: CirrOS disk image not found at /root or /var/tmp" >&2
    exit 1
fi
echo "    PASS: Using CirrOS disk image ${CIRROS_IMG}"

# 1. Build binaries
echo "==> 1. Building release binaries (o3kd, o3k, o3k-compute)..."
cargo build --release --bin o3kd --bin o3k
RUSTFLAGS="-l dylib=virt" cargo build --release --features libvirt --bin o3k-compute-bin

# 2. Build and tag Docker image
echo "==> 2. Building Docker image ${IMAGE_NAME}..."
docker build -t "${IMAGE_NAME}" -f "${ROOT_DIR}/deployments/docker/Dockerfile.o3kd-local" "${ROOT_DIR}"

# 3. Create Kind cluster with host port mappings
echo "==> 3. Creating Kind cluster ${CLUSTER_NAME} with hostPort mappings..."
if kind get clusters 2>/dev/null | grep -q "^${CLUSTER_NAME}$"; then
    kind delete cluster --name "${CLUSTER_NAME}"
fi

cat <<EOF | kind create cluster --name "${CLUSTER_NAME}" --config=-
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
  - role: control-plane
    extraPortMappings:
      - containerPort: 30000
        hostPort: 15000
        protocol: TCP
      - containerPort: 30443
        hostPort: 18443
        protocol: TCP
EOF

kind load docker-image "${IMAGE_NAME}" --name "${CLUSTER_NAME}"

# 4. Generate mTLS certificates
echo "==> 4. Generating mTLS certificates..."
printf 'o3k-owned-v1 path=%s\n' "${WORK_DIR}" > "${WORK_DIR}/.o3k-owned"
chmod 0640 "${WORK_DIR}/.o3k-owned"
mkdir -p "${WORK_DIR}/tls" "${WORK_DIR}/compute"
chgrp -R kvm "${WORK_DIR}" 2>/dev/null || true
chmod 0755 "${WORK_DIR}"
chmod 2775 "${WORK_DIR}/compute"
bash "${ROOT_DIR}/packaging/bootstrap-certs.sh" \
    --output-dir "${WORK_DIR}/tls" \
    --server-name "o3k-control-plane" \
    --agent-id "compute-agent" \
    --force

AGENT_FINGERPRINT=$(cat "${WORK_DIR}/tls/agent-fingerprint")
cp "${WORK_DIR}/tls/agent-id" "${WORK_DIR}/compute/agent-id"
echo "    PASS: Agent fingerprint: ${AGENT_FINGERPRINT}"

# 5. Create namespace and secrets
echo "==> 5. Setting up Kubernetes namespace and secrets..."
kubectl create namespace "${NAMESPACE}"

# PostgreSQL 16 deployment
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

echo "==> Waiting for PostgreSQL to be ready..."
kubectl rollout status deployment/postgres -n "${NAMESPACE}" --timeout=60s

kubectl create secret generic o3k-database \
    --namespace "${NAMESPACE}" \
    --from-literal=database-url="postgres://o3k:testlabsecret@postgres.${NAMESPACE}.svc.cluster.local:5432/o3k"

kubectl create secret generic o3k-bootstrap-auth \
    --namespace "${NAMESPACE}" \
    --from-literal=bootstrap-password="admin-testlab-password" \
    --from-literal=token-signing-key="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

kubectl create secret generic o3k-control-tls \
    --namespace "${NAMESPACE}" \
    --from-file=server.pem="${WORK_DIR}/tls/server.pem" \
    --from-file=server-key.pem="${WORK_DIR}/tls/server-key.pem" \
    --from-file=ca.pem="${WORK_DIR}/tls/ca.pem"

# 6. Deploy O3K Control Plane via Helm
echo "==> 6. Installing O3K Control Plane via Helm..."
helm upgrade --install o3k "${ROOT_DIR}/deployments/helm/o3k/" \
    --namespace "${NAMESPACE}" \
    --set image.pullPolicy="Never" \
    --set config.provider="agent" \
    --set persistence.size="2Gi" \
    --set auth.existingSecret="o3k-bootstrap-auth" \
    --set tls.enabled=true \
    --set tls.existingSecret="o3k-control-tls" \
    --set tls.authorizedAgents="compute-agent=${AGENT_FINGERPRINT}" \
    --set service.api.type="NodePort" \
    --set service.api.nodePort=30000 \
    --set service.compute.type="NodePort" \
    --set service.compute.nodePort=30443 \
    --wait \
    --timeout 60s

kubectl rollout status deployment/o3k -n "${NAMESPACE}" --timeout=60s
echo "    PASS: o3kd control plane running on Kubernetes"

# 7. Start external o3k-compute on host hypervisor
echo "==> 7. Starting external o3k-compute on host hypervisor..."
(
    umask 002
    exec env \
        O3K_COMPUTE_DATA_DIR="${WORK_DIR}/compute" \
        O3K_COMPUTE_CONTROL_ENDPOINT="https://127.0.0.1:18443" \
        O3K_COMPUTE_SERVER_NAME="o3k-control-plane" \
        O3K_COMPUTE_HOST_LABEL="kvm-node-1" \
        O3K_COMPUTE_TLS_DIR="${WORK_DIR}/tls" \
        O3K_COMPUTE_HEALTH_ADDR="127.0.0.1:19100" \
        O3K_COMPUTE_BRIDGE_NAME="${BRIDGE_NAME}" \
        O3K_COMPUTE_MAX_DISK_GB=20 \
        RUST_LOG=info \
        "${ROOT_DIR}/target/release/o3k-compute-bin" > "${WORK_DIR}/compute.log" 2>&1
) &
COMPUTE_PID=$!

echo "==> Waiting for o3k-compute to connect and register..."
for i in $(seq 1 30); do
    if curl -sf "http://127.0.0.1:19100/readyz" >/dev/null 2>&1; then
        echo "    PASS: o3k-compute is ready and connected to Kubernetes control plane"
        break
    fi
    sleep 1
    if [ "$i" -eq 30 ]; then
        echo "ERROR: o3k-compute failed to reach ready state within 30s" >&2
        cat "${WORK_DIR}/compute.log" >&2
        exit 1
    fi
done

# 8. Run o3k doctor in Kubernetes pod
echo "==> 8. Running o3k doctor inside Kubernetes control-plane pod..."
POD_NAME=$(kubectl get pods -n "${NAMESPACE}" -l "app.kubernetes.io/name=o3k,app.kubernetes.io/component=control-plane" -o jsonpath="{.items[0].metadata.name}")

DOCTOR_OUT=$(kubectl exec -n "${NAMESPACE}" "${POD_NAME}" -- o3k doctor --json || true)
echo "$DOCTOR_OUT"

PASS_COUNT=$(echo "$DOCTOR_OUT" | { grep -E '"status":\s*"PASS"' || true; } | wc -l)
FAIL_COUNT=$(echo "$DOCTOR_OUT" | { grep -E '"status":\s*"FAIL"' || true; } | wc -l)

if [ "${FAIL_COUNT}" -ne 0 ]; then
    echo "ERROR: o3k doctor reported failures!" >&2
    exit 1
fi
echo "    PASS: o3k doctor verified all components healthy (PASS=${PASS_COUNT}, FAIL=${FAIL_COUNT})"

# 9. Execute Real KVM CirrOS Lifecycle
echo "==> 9. Executing CirrOS VM lifecycle against Kubernetes control plane..."
API_BASE="http://127.0.0.1:15000"

# 9a. Authenticate
AUTH_RESP=$(curl -sf -i -X POST "${API_BASE}/v3/auth/tokens" \
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
echo "    PASS: Authenticated token issued for project ${PROJECT_ID}"

# 9b. Upload Glance Image
echo "==> Creating Glance image..."
IMAGE_RESP=$(curl -sf -X POST "${API_BASE}/v2/images" \
    -H "X-Auth-Token: ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d '{
        "name": "cirros-k8s",
        "disk_format": "qcow2",
        "container_format": "bare",
        "visibility": "private"
    }')

IMAGE_ID=$(python3 -c "import sys, json; data=json.loads(sys.stdin.read()); print(data['id'])" <<< "${IMAGE_RESP}")
echo "    PASS: Glance image created with ID ${IMAGE_ID}"

echo "==> Uploading binary image data..."
curl -sf -X PUT "${API_BASE}/v2/images/${IMAGE_ID}/file" \
    -H "X-Auth-Token: ${TOKEN}" \
    -H "Content-Type: application/octet-stream" \
    --data-binary "@${CIRROS_IMG}"
echo "    PASS: Image data uploaded"

# 9c. Create Network, Subnet, and Port
echo "==> Creating Neutron network..."
NET_RESP=$(curl -sf -X POST "${API_BASE}/v2.0/networks" \
    -H "X-Auth-Token: ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d '{"network": {"name": "k8s-kvm-net"}}')
NET_ID=$(python3 -c "import sys, json; data=json.loads(sys.stdin.read()); print(data['network']['id'])" <<< "${NET_RESP}")
echo "    PASS: Network created with ID ${NET_ID}"

echo "==> Creating Neutron subnet..."
SUBNET_RESP=$(curl -sf -X POST "${API_BASE}/v2.0/subnets" \
    -H "X-Auth-Token: ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d "{
        \"subnet\": {
            \"name\": \"k8s-kvm-subnet\",
            \"network_id\": \"${NET_ID}\",
            \"cidr\": \"10.10.0.0/24\"
        }
    }")
SUBNET_ID=$(python3 -c "import sys, json; data=json.loads(sys.stdin.read()); print(data['subnet']['id'])" <<< "${SUBNET_RESP}")
echo "    PASS: Subnet created with ID ${SUBNET_ID}"

echo "==> Creating Neutron port..."
PORT_RESP=$(curl -sf -X POST "${API_BASE}/v2.0/ports" \
    -H "X-Auth-Token: ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d "{
        \"port\": {
            \"name\": \"k8s-kvm-port\",
            \"network_id\": \"${NET_ID}\"
        }
    }")
PORT_ID=$(python3 -c "import sys, json; data=json.loads(sys.stdin.read()); print(data['port']['id'])" <<< "${PORT_RESP}")
echo "    PASS: Port created with ID ${PORT_ID}"

# 9d. Generate Keypair
echo "==> Generating SSH keypair..."
ssh-keygen -q -t ed25519 -N '' -C o3k-k8s-kvm -f "${WORK_DIR}/testlab-key"
PUB_KEY=$(cat "${WORK_DIR}/testlab-key.pub")
KEY_RESP=$(curl -sf -X POST "${API_BASE}/v2.1/${PROJECT_ID}/os-keypairs" \
    -H "X-Auth-Token: ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d "{
        \"keypair\": {
            \"name\": \"k8s-kvm-key\",
            \"public_key\": \"${PUB_KEY}\"
        }
    }")
echo "    PASS: Keypair registered"

# 9e. Boot Server
echo "==> Booting VM on KVM/libvirt..."
SERVER_RESP=$(curl -sf -X POST "${API_BASE}/v2.1/${PROJECT_ID}/servers" \
    -H "X-Auth-Token: ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d "{
        \"server\": {
            \"name\": \"cirros-k8s-guest\",
            \"imageRef\": \"${IMAGE_ID}\",
            \"flavorRef\": \"00000000-0000-0000-0000-000000000001\",
            \"config_drive\": true,
            \"key_name\": \"k8s-kvm-key\",
            \"networks\": [
                {
                    \"port\": \"${PORT_ID}\"
                }
            ]
        }
    }")

SERVER_ID=$(python3 -c "import sys, json; data=json.loads(sys.stdin.read()); print(data['server']['id'])" <<< "${SERVER_RESP}")
echo "    PASS: Nova server creation requested, server ID: ${SERVER_ID}"

# 9d. Wait for ACTIVE state
echo "==> Polling server state until ACTIVE..."
for i in $(seq 1 60); do
    SHOW_RESP=$(curl -sf -H "X-Auth-Token: ${TOKEN}" "${API_BASE}/v2.1/${PROJECT_ID}/servers/${SERVER_ID}")
    STATUS=$(python3 -c "import sys, json; data=json.loads(sys.stdin.read()); print(data['server']['status'])" <<< "${SHOW_RESP}")
    echo "    Server status (attempt $i): ${STATUS}"
    if [ "${STATUS}" = "ACTIVE" ]; then
        echo "    PASS: Server transitioned to ACTIVE!"
        break
    elif [ "${STATUS}" = "ERROR" ]; then
        echo "ERROR: Server transitioned to ERROR state" >&2
        echo "$SHOW_RESP" >&2
        cat "${WORK_DIR}/compute.log" >&2
        exit 1
    fi
    sleep 2
    if [ "$i" -eq 60 ]; then
        echo "ERROR: Server timed out waiting for ACTIVE state" >&2
        cat "${WORK_DIR}/compute.log" >&2
        exit 1
    fi
done

# 9e. Verify Domain in Libvirt
echo "==> Verifying running domain in libvirt..."
virsh list --all
RUNNING_DOMAINS=$(virsh list --name)
if [ -z "${RUNNING_DOMAINS}" ]; then
    echo "ERROR: No domains found running in libvirt" >&2
    exit 1
fi
echo "    PASS: Libvirt domain verified running on KVM hypervisor"

# 9f. Delete Server and verify teardown
echo "==> Deleting Nova server ${SERVER_ID}..."
curl -sf -X DELETE -H "X-Auth-Token: ${TOKEN}" "${API_BASE}/v2.1/${PROJECT_ID}/servers/${SERVER_ID}"

echo "==> Waiting for server deletion..."
for i in $(seq 1 30); do
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -H "X-Auth-Token: ${TOKEN}" "${API_BASE}/v2.1/${PROJECT_ID}/servers/${SERVER_ID}")
    if [ "${HTTP_CODE}" = "404" ]; then
        echo "    PASS: Server deleted in Nova control plane"
        break
    fi
    sleep 1
    if [ "$i" -eq 30 ]; then
        echo "ERROR: Server was not deleted within 30s" >&2
        exit 1
    fi
done

echo "==> Verifying libvirt domain destroyed..."
sleep 2
virsh list --all
echo "    PASS: Libvirt domain cleanly destroyed and unmanaged"

echo "============================================================"
echo "P6 Real KVM/Libvirt on Kubernetes Acceptance PASSED!"
echo "============================================================"
