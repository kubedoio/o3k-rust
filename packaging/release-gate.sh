#!/usr/bin/env bash
set -Eeuo pipefail

OUTPUT=release-evidence.json
E2E=
INSTALL_UBUNTU=
INSTALL_DEBIAN=
RECOVERY=
BENCHMARK=
while (($#)); do
  case "$1" in
    --e2e) E2E="${2:?missing E2E artifact}"; shift 2;;
    --install-ubuntu) INSTALL_UBUNTU="${2:?missing Ubuntu artifact}"; shift 2;;
    --install-debian) INSTALL_DEBIAN="${2:?missing Debian artifact}"; shift 2;;
    --recovery) RECOVERY="${2:?missing recovery artifact}"; shift 2;;
    --benchmark) BENCHMARK="${2:?missing benchmark artifact}"; shift 2;;
    --output) OUTPUT="${2:?missing output path}"; shift 2;;
    *) echo "unknown option: $1" >&2; exit 2;;
  esac
done
export E2E INSTALL_UBUNTU INSTALL_DEBIAN RECOVERY BENCHMARK OUTPUT
python3 <<'PY'
import json, os, sys
required = {
    "real_libvirt_e2e": (os.environ["E2E"], "openstack-cli-e2e"),
    "clean_ubuntu_install": (os.environ["INSTALL_UBUNTU"], "clean-install"),
    "clean_debian_install": (os.environ["INSTALL_DEBIAN"], "clean-install"),
    "failure_recovery": (os.environ["RECOVERY"], "failure-recovery"),
}
optional = {"benchmark": (os.environ["BENCHMARK"], "benchmark")}
errors = []
evidence = {}
paths = {}
for name, (path, artifact_type) in {**required, **optional}.items():
    if not path:
        errors.append(f"{name}: artifact path was not supplied")
        continue
    real_path = os.path.realpath(path)
    if real_path in paths:
        errors.append(f"{name}: artifact reuses {paths[real_path]}; each gate input must be distinct")
        continue
    paths[real_path] = name
    try:
        with open(path, encoding="utf-8") as stream:
            value = json.load(stream)
    except (OSError, json.JSONDecodeError) as error:
        errors.append(f"{name}: cannot read JSON artifact ({error})")
        continue
    if not isinstance(value, dict):
        errors.append(f"{name}: artifact root must be an object")
        continue
    status = value.get("status")
    evidence[name] = {
        "path": path,
        "artifact_type": value.get("artifact_type"),
        "profile": value.get("profile"),
        "status": status,
    }
    if value.get("artifact_type") != artifact_type:
        errors.append(f"{name}: artifact_type must be {artifact_type!r}")
    if value.get("profile") != "libvirt":
        errors.append(f"{name}: profile must be 'libvirt'")
    if value.get("redacted") is not True:
        errors.append(f"{name}: redacted must be true")
    if not isinstance(value.get("finished_at"), int):
        errors.append(f"{name}: finished_at must be an integer epoch timestamp")
    cleanup = value.get("cleanup")
    if not isinstance(cleanup, dict) or cleanup.get("status") != "passed":
        errors.append(f"{name}: cleanup.status must be 'passed'")
    if name == "benchmark":
        if status != "measured":
            errors.append(f"{name}: status must be measured, got {status!r}")
        guest = value.get("guest_and_libvirt")
        if not isinstance(guest, dict) or guest.get("status") != "measured":
            errors.append(f"{name}: guest_and_libvirt.status must be measured")
        targets = value.get("targets_evaluated")
        if not isinstance(targets, dict) or not targets or not all(targets.values()):
            errors.append(f"{name}: all benchmark targets must evaluate true")
    elif status != "passed":
        errors.append(f"{name}: status must be passed, got {status!r}")
    if name == "real_libvirt_e2e":
        lifecycle = value.get("lifecycle")
        expected = {"create", "show", "list", "stop", "start", "reboot", "console", "delete"}
        if not isinstance(lifecycle, dict) or set(lifecycle) != expected or not all(lifecycle.values()):
            errors.append(f"{name}: lifecycle must prove create/show/list/stop/start/reboot/console/delete")
        if value.get("public_api_only") is not True:
            errors.append(f"{name}: public_api_only must be true")
    if name == "failure_recovery":
        failures = value.get("failures")
        if not isinstance(failures, list) or not failures:
            errors.append(f"{name}: failures must list exercised recovery scenarios")
    if name in {"clean_ubuntu_install", "clean_debian_install"}:
        expected_distro = "ubuntu" if name.endswith("ubuntu_install") else "debian"
        if value.get("distro") != expected_distro:
            errors.append(f"{name}: distro must be {expected_distro!r}")
        install = value.get("install")
        if not isinstance(install, dict) or install.get("status") != "passed":
            errors.append(f"{name}: install.status must be 'passed'")
report = {"release": "v0.2.0-alpha.1", "profile": "libvirt", "status": "ready" if not errors else "blocked", "evidence": evidence, "errors": errors, "tag_created": False}
with open(os.environ["OUTPUT"], "w", encoding="utf-8") as stream:
    json.dump(report, stream, indent=2, sort_keys=True); stream.write("\n")
if errors:
    print("release gate blocked; see " + os.environ["OUTPUT"], file=sys.stderr)
    for error in errors: print("- " + error, file=sys.stderr)
    sys.exit(1)
print("release gate passed; tag creation remains an explicit operator action")
PY
