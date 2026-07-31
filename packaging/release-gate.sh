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
    "real_libvirt_e2e": os.environ["E2E"],
    "clean_ubuntu_install": os.environ["INSTALL_UBUNTU"],
    "clean_debian_install": os.environ["INSTALL_DEBIAN"],
    "failure_recovery": os.environ["RECOVERY"],
}
optional = {"benchmark": os.environ["BENCHMARK"]}
errors = []
evidence = {}
for name, path in {**required, **optional}.items():
    if not path:
        errors.append(f"{name}: artifact path was not supplied")
        continue
    try:
        with open(path, encoding="utf-8") as stream:
            value = json.load(stream)
    except (OSError, json.JSONDecodeError) as error:
        errors.append(f"{name}: cannot read JSON artifact ({error})")
        continue
    status = value.get("status")
    evidence[name] = {"path": path, "status": status}
    if name == "benchmark":
        if status != "measured": errors.append(f"{name}: status must be measured, got {status!r}")
    elif status != "passed":
        errors.append(f"{name}: status must be passed, got {status!r}")
report = {"release": "v0.2.0-alpha.1", "profile": "libvirt", "status": "ready" if not errors else "blocked", "evidence": evidence, "errors": errors, "tag_created": False}
with open(os.environ["OUTPUT"], "w", encoding="utf-8") as stream:
    json.dump(report, stream, indent=2, sort_keys=True); stream.write("\n")
if errors:
    print("release gate blocked; see " + os.environ["OUTPUT"], file=sys.stderr)
    for error in errors: print("- " + error, file=sys.stderr)
    sys.exit(1)
print("release gate passed; tag creation remains an explicit operator action")
PY
