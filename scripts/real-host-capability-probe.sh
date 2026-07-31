#!/usr/bin/env bash
set -Eeuo pipefail

# Read-only eligibility probe for the protected real-host workflow. It never
# installs, starts, stops, deletes, or changes host resources.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${O3K_REAL_HOST_ARTIFACT_DIR:-${ROOT_DIR}/target/real-host-workflow-artifacts}"
OUTPUT_PATH="${O3K_REAL_HOST_CAPABILITY_OUTPUT:-${ARTIFACT_DIR}/runner-capabilities.json}"
DISK_PATH="${O3K_REAL_HOST_DISK_PATH:-${ROOT_DIR}}"
KVM_PATH="${O3K_REAL_HOST_KVM_PATH:-/dev/kvm}"
EXPECTED_URI="qemu:///system"
MIN_FREE_BYTES="${O3K_REAL_HOST_MIN_FREE_BYTES:-10737418240}"
EXPECTED_LABELS="self-hosted,linux,x64,kvm,libvirt,o3k-testlab"

mkdir -p "$(dirname "${OUTPUT_PATH}")"
# Do not leave an older passing artifact available if the probe cannot start or
# is interrupted before it publishes a replacement. Removing a symlink removes
# only the link, never its target.
rm -f -- "${OUTPUT_PATH}"

python3 - "${OUTPUT_PATH}" "${DISK_PATH}" "${KVM_PATH}" "${MIN_FREE_BYTES}" "${EXPECTED_LABELS}" \
    "${O3K_REAL_HOST_WORKFLOW_RUN_ID:-}" "${O3K_REAL_HOST_WORKFLOW_RUN_ATTEMPT:-}" \
    "${GITHUB_SHA:-}" <<'PY'
import json
import os
import pwd
import shutil
import stat
import subprocess
import sys
import tempfile
import time

(
    output_path,
    disk_path,
    kvm_path,
    minimum_free,
    expected_labels,
    workflow_run_id,
    workflow_run_attempt,
    source_commit,
) = sys.argv[1:]
errors = []
skips = []

def command_available(name):
    return shutil.which(name) is not None

def command_succeeds(args):
    try:
        return subprocess.run(args, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL, check=False, timeout=10).returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False

tools = {name: command_available(name) for name in (
    "virsh", "qemu-img", "ip", "dnsmasq", "openstack"
)}
config_drive_candidates = ("cloud-localds", "genisoimage", "mkisofs", "xorriso")
config_drive_tools = {name: command_available(name) for name in config_drive_candidates}
config_drive_available = any(config_drive_tools.values())

kvm_present = os.path.exists(kvm_path)
kvm_readable = os.access(kvm_path, os.R_OK)
kvm_character_device = False
try:
    kvm_character_device = stat.S_ISCHR(os.stat(kvm_path).st_mode)
except OSError:
    pass

libvirt_uri = tools["virsh"] and command_succeeds(["virsh", "-c", "qemu:///system", "uri"])
disk_free_bytes = None
disk_path_valid = os.path.isdir(disk_path)
if disk_path_valid:
    try:
        disk_free_bytes = shutil.disk_usage(disk_path).free
    except OSError:
        disk_path_valid = False
try:
    minimum_free_bytes = int(minimum_free)
    if minimum_free_bytes < 0:
        raise ValueError
except ValueError:
    errors.append("invalid_min_free_bytes")
    minimum_free_bytes = 0

runner_uid = os.getuid()
non_root = runner_uid != 0
service_account = os.environ.get("O3K_REAL_HOST_SERVICE_ACCOUNT", "")
service_account_configured = bool(service_account)
service_account_exists = False
service_account_matches = False
if service_account_configured:
    try:
        service_account_exists = pwd.getpwnam(service_account).pw_uid != 0
        service_account_matches = service_account_exists and pwd.getpwnam(service_account).pw_uid == runner_uid
    except KeyError:
        pass
if not non_root:
    skips.append("runner_is_root")
if not service_account_configured:
    skips.append("service_account_not_declared")
elif not service_account_exists:
    errors.append("service_account_unavailable")
elif not service_account_matches:
    errors.append("service_account_mismatch")

actual_labels_raw = os.environ.get("O3K_REAL_HOST_RUNNER_LABELS", "")
actual_labels = [item.strip() for item in actual_labels_raw.split(",") if item.strip()]
expected = [item for item in expected_labels.split(",") if item]
labels_declared = bool(actual_labels_raw)
labels_exact = actual_labels == expected
if not labels_declared:
    skips.append("runner_labels_not_declared")
elif not labels_exact:
    errors.append("runner_labels_mismatch")

image_path = os.environ.get("O3K_TESTLAB_IMAGE_PATH", "")
image_path_absolute = os.path.isabs(image_path)
image_path_regular = os.path.isfile(image_path) if image_path else False
if not image_path:
    skips.append("image_path_not_declared")
elif not image_path_absolute or not image_path_regular:
    errors.append("image_path_invalid")

checks = {
    "tools": tools,
    "config_drive": {"available": config_drive_available, "tools": config_drive_tools},
    "kvm": {"path_is_exact": kvm_path == "/dev/kvm", "present": kvm_present,
            "readable": kvm_readable, "character_device": kvm_character_device},
    "libvirt": {"uri_expected": "qemu:///system", "system_uri_available": libvirt_uri},
    "runner": {"non_root": non_root, "uid_is_dedicated": non_root,
               "service_account_declared": service_account_configured,
               "service_account_exists": service_account_exists,
               "service_account_matches_user": service_account_matches,
               "labels_declared": labels_declared, "labels_exact": labels_exact,
               "expected_labels": expected},
    "disk": {"path_is_directory": disk_path_valid, "minimum_free_bytes": minimum_free_bytes,
             "free_bytes": disk_free_bytes,
             "enough_free_space": disk_free_bytes is not None and disk_free_bytes >= minimum_free_bytes},
    "config": {"image_path_declared": bool(image_path), "image_path_absolute": image_path_absolute,
               "image_path_regular_file": image_path_regular},
}

required_missing = []
required_missing.extend(name for name, present in tools.items() if not present)
if not config_drive_available:
    required_missing.append("config-drive-tooling")
if kvm_path != "/dev/kvm" or not kvm_present or not kvm_readable or not kvm_character_device:
    required_missing.append("/dev/kvm")
if not libvirt_uri:
    required_missing.append("qemu:///system")
if not disk_path_valid or disk_free_bytes is None or disk_free_bytes < minimum_free_bytes:
    required_missing.append("disk-space")

if errors:
    status = "failed"
    reason = errors[0]
elif required_missing or skips:
    status = "skipped"
    reason = ("missing_required_capability" if required_missing else skips[0])
else:
    status = "passed"
    reason = "all_required_capabilities_present"

result = {
    "artifact_type": "runner-capabilities",
    "schema_version": 1,
    "status": status,
    "reason": reason,
    "redacted": True,
    "required_missing": sorted(set(required_missing)),
    "checks": checks,
    "finished_at": int(time.time()),
}

# The workflow identity is intentionally metadata, not a secret. It prevents a
# persistent self-hosted workspace from reusing an artifact from another run or
# retry. Local portable tests may omit it; the protected workflow always sets it.
if workflow_run_id:
    result["workflow_run_id"] = workflow_run_id
if workflow_run_attempt:
    result["workflow_run_attempt"] = workflow_run_attempt
if source_commit:
    result["source_commit"] = source_commit

directory = os.path.dirname(output_path) or "."
descriptor, temporary = tempfile.mkstemp(prefix=".runner-capabilities.", dir=directory,
                                          text=True)
try:
    with os.fdopen(descriptor, "w", encoding="utf-8") as output:
        json.dump(result, output, indent=2, sort_keys=True)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, output_path)
except BaseException:
    try:
        os.unlink(temporary)
    except FileNotFoundError:
        pass
    raise

if status == "failed":
    raise SystemExit(1)
PY

status="$(python3 - "${OUTPUT_PATH}" <<'PY'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["status"])
PY
)"
echo "real-host capability probe: ${status}"
