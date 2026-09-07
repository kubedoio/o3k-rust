#!/usr/bin/env bash
set -Eeuo pipefail

# Creates the run-owned source workload and sentinel inventory in the real
# DevStack cloud created by p14_9c_openstack_source.sh.  All IDs and secrets
# stay below the protected runtime state directory.

STATE_ROOT="${O3K_P14_9C_SOURCE_STATE:-/var/lib/o3k/p14-9c-source}"
ENV_FILE="$STATE_ROOT/source.env"
INVENTORY="$STATE_ROOT/inventory.env"
IMAGE_URL="https://download.cirros-cloud.net/0.6.3/cirros-0.6.3-x86_64-disk.img"
IMAGE_SHA256="7d6355852aeb6dbcd191bcda7cd74f1536cfe5cbf8a10495a7283a8396e4b75b"
IMAGE_FILE="$STATE_ROOT/cirros-0.6.3-x86_64-disk.img"
KEY_FILE="$STATE_ROOT/project-a-ssh-ed25519"

die() { echo "P14.9C workload: $*" >&2; exit 2; }
[[ -r "$ENV_FILE" ]] || die "missing protected source environment: $ENV_FILE"
command -v openstack >/dev/null || die "missing openstack client"
command -v curl >/dev/null || die "missing curl"
command -v sha256sum >/dev/null || die "missing sha256sum"
command -v jq >/dev/null || die "missing jq"
command -v ssh >/dev/null || die "missing ssh"
command -v sshpass >/dev/null || die "missing sshpass"
command -v openssl >/dev/null || die "missing openssl"
command -v ssh-keygen >/dev/null || die "missing ssh-keygen"
[[ "$(id -u)" == 0 ]] || die "must run as root"

set -a
. "$ENV_FILE"
set +a
export OS_AUTH_URL="$O3K_P14_SOURCE_AUTH_URL" OS_USERNAME=admin
export OS_PASSWORD="${O3K_P14_SOURCE_ADMIN_PASSWORD:-$O3K_P14_SOURCE_PASSWORD}" OS_USER_DOMAIN_NAME=Default
export OS_PROJECT_NAME=admin OS_PROJECT_DOMAIN_NAME=Default
export OS_REGION_NAME="${O3K_P14_SOURCE_REGION:-RegionOne}"
export OS_INTERFACE=public OS_IDENTITY_API_VERSION=3
export OS_CLIENT_TIMEOUT=15

os() { openstack --os-cloud '' "$@"; }
os_project() {
    local project="$1" user="$2" password="$3"; shift 3
    env -u OS_PROJECT_NAME -u OS_PROJECT_DOMAIN_NAME -u OS_TENANT_NAME \
        OS_USERNAME="$user" OS_PASSWORD="$password" OS_PROJECT_ID="$project" \
        openstack --os-cloud '' "$@"
}
create_password() { openssl rand -hex 24; }
project_id() { os project show "$1" -f value -c id; }
set_env() {
    local key="$1" value="$2" tmp
    tmp="$(mktemp "${ENV_FILE}.XXXXXX")"
    chmod 600 "$tmp"
    awk -F= -v key="$key" '$1 != key' "$ENV_FILE" >"$tmp"
    printf '%s=%s\n' "$key" "$value" >>"$tmp"
    mv -f "$tmp" "$ENV_FILE"
}

create() {
    echo "workload-step: keystone"
    os token issue -f value -c id >/dev/null || die "Keystone token failed"
    echo "workload-step: image artifact"
    [[ -f "$IMAGE_FILE" ]] || curl -fL --retry 3 -o "$IMAGE_FILE" "$IMAGE_URL"
    [[ "$(sha256sum "$IMAGE_FILE" | awk '{print $1}')" == "$IMAGE_SHA256" ]] || die "CirrOS checksum mismatch"

    echo "workload-step: projects"
    os project show p14-source-a >/dev/null 2>&1 || os project create p14-source-a --description p14-9c-migration-owned >/dev/null
    os project show p14-source-b >/dev/null 2>&1 || os project create p14-source-b --description p14-9c-sentinel >/dev/null
    A_PROJECT="$(project_id p14-source-a)"; B_PROJECT="$(project_id p14-source-b)"
    A_PASSWORD="$(create_password)"; B_PASSWORD="$(create_password)"
    echo "workload-step: users"
    os user show p14-source-a-user >/dev/null 2>&1 && os user set --password "$A_PASSWORD" p14-source-a-user || os user create p14-source-a-user --project "$A_PROJECT" --password "$A_PASSWORD" >/dev/null
    os user show p14-source-b-user >/dev/null 2>&1 && os user set --password "$B_PASSWORD" p14-source-b-user || os user create p14-source-b-user --project "$B_PROJECT" --password "$B_PASSWORD" >/dev/null
    os role add --project "$A_PROJECT" --user p14-source-a-user member 2>/dev/null || true
    os role add --project "$B_PROJECT" --user p14-source-b-user member 2>/dev/null || true
    set_env O3K_P14_SOURCE_PROJECT_A_PASSWORD "$A_PASSWORD"
    set_env O3K_P14_SOURCE_PROJECT_B_PASSWORD "$B_PASSWORD"
    project_a() { os_project "$A_PROJECT" p14-source-a-user "$A_PASSWORD" "$@"; }
    project_b() { os_project "$B_PROJECT" p14-source-b-user "$B_PASSWORD" "$@"; }

    # The migration source must expose a project-owned image.  A public
    # admin-owned catalog image is deliberately not workload authority and is
    # filtered from the source snapshot by the acceptance adapter.
    echo "workload-step: project-a image"
    project_a image show p14-cirros >/dev/null 2>&1 || project_a image create p14-cirros \
        --file "$IMAGE_FILE" --disk-format qcow2 --container-format bare --private >/dev/null
    IMAGE_ID="$(project_a image show p14-cirros -f value -c id)"

    echo "workload-step: project-a network"
    project_a network show p14-source-a-net >/dev/null 2>&1 || project_a network create p14-source-a-net >/dev/null
    project_a subnet show p14-source-a-subnet >/dev/null 2>&1 || project_a subnet create --network p14-source-a-net --subnet-range 10.240.1.0/24 p14-source-a-subnet >/dev/null
    EXT_NET="$(os network list --external -f value -c ID | head -n 1)"; [[ -n "$EXT_NET" ]] || die "no external network"
    project_a router show p14-source-a-router >/dev/null 2>&1 || project_a router create p14-source-a-router >/dev/null
    project_a router set --external-gateway "$EXT_NET" p14-source-a-router
    project_a router add subnet p14-source-a-router p14-source-a-subnet 2>/dev/null || true
    if ! project_a keypair show p14-source-a-key >/dev/null 2>&1; then
        [[ -f "$KEY_FILE" ]] || ssh-keygen -q -t ed25519 -N '' -f "$KEY_FILE" -C p14-9c-project-a
        chmod 600 "$KEY_FILE"
        project_a keypair create --public-key "$KEY_FILE.pub" p14-source-a-key >/dev/null
    fi
    project_a security group show p14-source-a-sg >/dev/null 2>&1 || project_a security group create p14-source-a-sg >/dev/null
    project_a security group rule create --ingress --protocol icmp p14-source-a-sg >/dev/null 2>&1 || true
    project_a security group rule create --ingress --protocol tcp --dst-port 22 p14-source-a-sg >/dev/null 2>&1 || true
    echo "workload-step: project-a compute"
    project_a volume show p14-source-a-volume >/dev/null 2>&1 || project_a volume create --size 2 p14-source-a-volume >/dev/null
    if ! project_a server show p14-source-a-server >/dev/null 2>&1; then
        project_a server create --image "$IMAGE_ID" --flavor m1.small --network p14-source-a-net --key-name p14-source-a-key --security-group p14-source-a-sg p14-source-a-server >/dev/null
    else
        CURRENT_IMAGE_ID="$(project_a server show p14-source-a-server -f json | jq -r 'if (.image | type) == "object" then (.image.id // empty) else (.image // empty) end')"
        if [[ "$CURRENT_IMAGE_ID" != "$IMAGE_ID" ]]; then
            project_a server rebuild --image "$IMAGE_ID" p14-source-a-server >/dev/null
            for _ in {1..60}; do
                [[ "$(project_a server show p14-source-a-server -f value -c status)" == "ACTIVE" ]] && break
                sleep 2
            done
        fi
    fi
    SERVER_STATE="$(project_a server show p14-source-a-server -f value -c status)"
    # A newly created server is normally BUILD.  Do not issue a second
    # lifecycle mutation until Nova has reported a stable state; doing so
    # turns a harmless rerun into a 409 race with the build operation.
    if [[ "$SERVER_STATE" == "BUILD" || "$SERVER_STATE" == "SPAWNING" ]]; then
        for _ in {1..90}; do
            SERVER_STATE="$(project_a server show p14-source-a-server -f value -c status)"
            [[ "$SERVER_STATE" != "BUILD" && "$SERVER_STATE" != "SPAWNING" ]] && break
            sleep 2
        done
    fi
    if [[ "$SERVER_STATE" != "ACTIVE" ]]; then
        project_a server start p14-source-a-server >/dev/null
        for _ in {1..60}; do
            SERVER_STATE="$(project_a server show p14-source-a-server -f value -c status)"
            [[ "$SERVER_STATE" == "ACTIVE" ]] && break
            sleep 2
        done
    fi
    [[ "$SERVER_STATE" == "ACTIVE" ]] || die "project-a server did not become active"
    echo "workload-step: project-a attachment-and-probe"
    project_a server add volume p14-source-a-server p14-source-a-volume 2>/dev/null || true
    for _ in {1..60}; do
        ATTACHMENT_STATE="$(project_a volume show p14-source-a-volume -f value -c status)"
        [[ "$ATTACHMENT_STATE" == "in-use" ]] && break
        sleep 2
    done
    [[ "$ATTACHMENT_STATE" == "in-use" ]] || die "project-a volume did not become attached"
    PORT_ID="$(project_a port list --server p14-source-a-server -f value -c ID | head -n 1)"
    [[ -n "$PORT_ID" ]] || die "project-a server has no tenant port"
    FIP_ID="$(project_a floating ip list --port "$PORT_ID" -f value -c ID | head -n 1)"
    if [[ -z "$FIP_ID" ]]; then
        FIP_ID="$(project_a floating ip create "$EXT_NET" -f value -c id)"
        project_a floating ip set --port "$PORT_ID" "$FIP_ID" >/dev/null
    fi
    FIP_ADDR="$(project_a floating ip show "$FIP_ID" -f value -c floating_ip_address)"
    [[ "$FIP_ADDR" =~ ^[0-9.]+$ ]] || die "project-a floating address is invalid"

    # Write a bounded deterministic payload through the guest and calculate
    # the expected digest locally.  The guest-side digest must agree before
    # the value is recorded in the protected run environment.
    payload="p14-9c-volume-proof:${A_PROJECT}:bounded-real-workload\n"
    expected_digest="$(printf '%b' "$payload" | sha256sum | awk '{print $1}')"
    guest_command="set -eu; mountpoint -q /mnt/p14-volume || (sudo mkfs.ext4 -F /dev/vdb >/dev/null 2>&1 && sudo mkdir -p /mnt/p14-volume && sudo mount /dev/vdb /mnt/p14-volume); printf '%b' '$payload' | sudo tee /mnt/p14-volume/p14-checksum-input >/dev/null; sudo sha256sum /mnt/p14-volume/p14-checksum-input | awk '{print \$1}'"
    observed_digest=""
    guest_ssh_opts=(-o ConnectTimeout=3 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null)
    guest_proxy="-o ProxyCommand=ssh -i $STATE_ROOT/ssh_ed25519 -o StrictHostKeyChecking=no stack@$O3K_P14_SOURCE_ALLOWED_HOSTS -W %h:%p"
    for _ in {1..30}; do
        observed_digest="$(ssh "${guest_ssh_opts[@]}" $guest_proxy -i "$KEY_FILE" cirros@"$FIP_ADDR" "$guest_command" 2>/dev/null || true)"
        if [[ "$observed_digest" != "$expected_digest" ]]; then
            # CirrOS 0.6.3's stock image may expose its documented disposable
            # console password while ignoring injected keys.  Keep the key
            # path first, and use that bounded fallback only for this real
            # guest probe; no credential is written to evidence.
            observed_digest="$(sshpass -p "${P14_SOURCE_GUEST_PASSWORD:-gocubsgo}" ssh "${guest_ssh_opts[@]}" $guest_proxy -o PreferredAuthentications=password -o PubkeyAuthentication=no cirros@"$FIP_ADDR" "$guest_command" 2>/dev/null || true)"
        fi
        [[ "$observed_digest" == "$expected_digest" ]] && break
        sleep 2
    done
    [[ "$observed_digest" == "$expected_digest" ]] || die "project-a guest volume checksum mismatch"

    echo "workload-step: project-b sentinels"
    project_b network show p14-source-b-net >/dev/null 2>&1 || project_b network create p14-source-b-net >/dev/null
    project_b volume show p14-source-b-sentinel >/dev/null 2>&1 || project_b volume create --size 1 p14-source-b-sentinel >/dev/null
    set_env O3K_P14_SOURCE_PROJECT_ID "$A_PROJECT"
    set_env O3K_P14_SOURCE_PROJECT_B "$B_PROJECT"
    set_env O3K_P14_SOURCE_USERNAME p14-source-a-user
    set_env O3K_P14_SOURCE_PASSWORD "$A_PASSWORD"
    set_env O3K_P14_SOURCE_PROJECT_B_USERNAME p14-source-b-user
    set_env O3K_P14_SOURCE_PROJECT_A_PASSWORD "$A_PASSWORD"
    set_env O3K_P14_SOURCE_PROJECT_B_PASSWORD "$B_PASSWORD"
    set_env O3K_P14_SOURCE_IMAGE_ID "$IMAGE_ID"
    set_env O3K_P14_SOURCE_EXTERNAL_NETWORK_ID "$EXT_NET"
    set_env P14_SOURCE_SSH_PRIVATE_KEY "$KEY_FILE"
    set_env P14_SOURCE_VOLUME_SHA256 "$expected_digest"
    set_env P14_SOURCE_FLOATING_IP "$FIP_ADDR"
    {
        printf 'project_a_id=%s\n' "$A_PROJECT"
        printf 'project_b_id=%s\n' "$B_PROJECT"
        printf 'project_a_network_id=%s\n' "$(project_a network show p14-source-a-net -f value -c id)"
        printf 'project_b_network_id=%s\n' "$(project_b network show p14-source-b-net -f value -c id)"
        printf 'project_a_volume_id=%s\n' "$(project_a volume show p14-source-a-volume -f value -c id)"
        printf 'project_b_volume_id=%s\n' "$(project_b volume show p14-source-b-sentinel -f value -c id)"
        printf 'project_a_server_id=%s\n' "$(project_a server show p14-source-a-server -f value -c id)"
        printf 'project_a_floating_ip=%s\n' "$FIP_ADDR"
    } >"$INVENTORY"
    chmod 600 "$INVENTORY"
    chmod 600 "$ENV_FILE"
    os project show p14-source-a -f value -c id >/dev/null
    os project show p14-source-b -f value -c id >/dev/null
    printf 'source workload created; protected environment: %s\n' "$ENV_FILE"
}

case "${1:-create}" in
    create) create ;;
    *) die "usage: $0 create" ;;
esac
