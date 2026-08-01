#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $# -eq 0 ]]; then
  DIST_ROOT="${O3K_RELEASE_DIST_DIR:-$ROOT_DIR/dist}"
  if [[ -L "$DIST_ROOT" || ( -e "$DIST_ROOT" && ! -d "$DIST_ROOT" ) ]]; then
    echo "SBOM dist root must be a real directory, not a symlink or special file" >&2
    exit 2
  fi
  mkdir -p -- "$DIST_ROOT"
  OUTPUT="$DIST_ROOT/sbom.spdx.json"
else
  OUTPUT="$1"
  mkdir -p -- "$(dirname "$OUTPUT")"
fi

METADATA_FILE="$(mktemp)"
trap 'rm -f "$METADATA_FILE"' EXIT
cargo metadata --manifest-path "$ROOT_DIR/Cargo.toml" --locked --format-version 1 >"$METADATA_FILE"
COMMIT="$(git -C "$ROOT_DIR" rev-parse HEAD)"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$ROOT_DIR" show -s --format=%ct HEAD)}"
export METADATA_FILE COMMIT OUTPUT SOURCE_DATE_EPOCH

python3 <<'PY'
import json
import os
from datetime import datetime, timezone

with open(os.environ["METADATA_FILE"], encoding="utf-8") as stream:
    metadata = json.load(stream)
packages = []
relationships = []
for index, package in enumerate(metadata["packages"]):
    spdx_id = f"SPDXRef-Package-{index}"
    source = package.get("source") or "NOASSERTION"
    license_name = package.get("license") or "NOASSERTION"
    packages.append({
        "SPDXID": spdx_id,
        "name": package["name"],
        "versionInfo": package["version"],
        "downloadLocation": source,
        "licenseConcluded": license_name,
        "licenseDeclared": license_name,
        "supplier": "NOASSERTION",
    })
    if package["id"] in metadata["workspace_members"]:
        relationships.append({
            "spdxElementId": "SPDXRef-DOCUMENT",
            "relationshipType": "DESCRIBES",
            "relatedSpdxElement": spdx_id,
        })

created = datetime.fromtimestamp(int(os.environ["SOURCE_DATE_EPOCH"]), timezone.utc)
document = {
    "spdxVersion": "SPDX-2.3",
    "dataLicense": "CC0-1.0",
    "SPDXID": "SPDXRef-DOCUMENT",
    "name": "o3k-rust dependency SBOM",
    "documentNamespace": f"https://github.com/kubedoio/o3k-rust/sbom/{os.environ['COMMIT']}",
    "creationInfo": {
        "created": created.isoformat().replace("+00:00", "Z"),
        "creators": ["Tool: o3k-rust packaging/make-sbom.sh"],
        "licenseListVersion": "3.23",
    },
    "packages": packages,
    "relationships": relationships,
    "annotations": [{
        "annotationDate": created.isoformat().replace("+00:00", "Z"),
        "annotationType": "OTHER",
        "annotator": "Tool: o3k-rust packaging/make-sbom.sh",
        "comment": f"source_commit={os.environ['COMMIT']}; workflow={os.environ.get('GITHUB_WORKFLOW', 'local')}",
        "SPDXID": "SPDXRef-DOCUMENT",
    }],
}
with open(os.environ["OUTPUT"], "w", encoding="utf-8") as stream:
    json.dump(document, stream, indent=2, sort_keys=True)
    stream.write("\n")
PY
echo "SBOM written to $OUTPUT"
