#!/usr/bin/env bash
# Tests for packaging/check-glibc-baseline.sh and its wiring in
# packaging/verify-release-bundle.sh.
#
# Release binaries must run on every advertised target. Debian 12 (bookworm)
# ships glibc 2.36, so a release binary built on a newer baseline (for
# example Ubuntu 24.04, whose glibc 2.38/2.39 symbols such as
# __isoc23_sscanf / pidfd_spawnp cannot exec on bookworm) must fail the check.
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-glibc-baseline.XXXXXX")"
trap 'rm -rf -- "$WORK_DIR"' EXIT
CHECKER="$ROOT_DIR/packaging/check-glibc-baseline.sh"

command -v gcc >/dev/null || { echo "gcc is required for the fixture binaries" >&2; exit 2; }

# Passing fixture: a plain C program only needs the ancient glibc floor
# (GLIBC_2.2.5 .. GLIBC_2.34), so it must pass the 2.36 baseline check.
printf '%s\n' '#include <stdio.h>' 'int main(void) { puts("ok"); return 0; }' \
  >"$WORK_DIR/pass.c"
gcc -o "$WORK_DIR/pass" "$WORK_DIR/pass.c"

# Failing fixture: sscanf compiled in C23 mode resolves to __isoc23_sscanf,
# which only exists in glibc >= 2.38, so the binary must fail the 2.36 check.
# The fixture can only be produced on a host whose glibc is >= 2.38; on older
# hosts the negative assertions are skipped with a notice.
printf '%s\n' '#include <stdio.h>' \
  'int main(void) { char buf[8]; sscanf("12", "%d", (int *)buf); return 0; }' \
  >"$WORK_DIR/fail.c"
HOST_GLIBC="$(ldd --version 2>/dev/null | awk 'NR == 1 { if (match($0, /[0-9]+\.[0-9]+/)) print substr($0, RSTART, RLENGTH); exit }')" || true
FAIL_FIXTURE=1
if ! gcc -std=c2x -o "$WORK_DIR/fail" "$WORK_DIR/fail.c" 2>/dev/null \
    && ! gcc -std=c23 -o "$WORK_DIR/fail" "$WORK_DIR/fail.c" 2>/dev/null; then
  FAIL_FIXTURE=0
  echo "skipping negative fixture: host glibc $HOST_GLIBC cannot build a GLIBC_2.38 binary" >&2
elif ! objdump -T "$WORK_DIR/fail" 2>/dev/null | grep 'GLIBC_2.38' >/dev/null; then
  FAIL_FIXTURE=0
  echo "skipping negative fixture: compiler did not emit a GLIBC_2.38 requirement" >&2
fi

# Sanity: the pass fixture must not accidentally require GLIBC_2.38.
if objdump -T "$WORK_DIR/pass" | grep 'GLIBC_2.3[89]' >/dev/null; then
  echo "pass fixture unexpectedly requires a new glibc" >&2
  exit 1
fi

# GLIBC_2.36-only binary passes the standalone check.
bash "$CHECKER" "$WORK_DIR/pass"

# A GLIBC_2.38 binary fails the standalone check with a message naming the
# offending version and the fix (rebuild on the Debian 12 baseline).
if (( FAIL_FIXTURE )); then
  if bash "$CHECKER" "$WORK_DIR/fail" 2>"$WORK_DIR/fail-output.txt"; then
    echo "glibc baseline check accepted a GLIBC_2.38 binary" >&2
    exit 1
  fi
  grep -q 'GLIBC_2.38' "$WORK_DIR/fail-output.txt"
  grep -qi 'debian 12' "$WORK_DIR/fail-output.txt"
fi

# The baseline is configurable so a stricter floor can be enforced.
if O3K_MAX_GLIBC=2.2 bash "$CHECKER" "$WORK_DIR/pass"; then
  echo "glibc baseline check ignored O3K_MAX_GLIBC" >&2
  exit 1
fi

# Non-ELF files (scripts, docs) are skipped, not rejected: the glibc floor
# applies to the shipped native binaries only.
printf '#!/usr/bin/env bash\necho helper\n' >"$WORK_DIR/helper"
bash "$CHECKER" "$WORK_DIR/helper"

# Usage errors exit 2.
if bash "$CHECKER" >/dev/null 2>&1; then
  echo "glibc baseline check accepted a missing binary argument" >&2
  exit 1
fi
if bash "$CHECKER" "$WORK_DIR/does-not-exist" >/dev/null 2>&1; then
  echo "glibc baseline check accepted a missing file" >&2
  exit 1
fi

# verify-release-bundle.sh wiring: a bundle whose bin/ holds a binary above
# the glibc floor must be rejected; a compliant bundle must pass.
BUNDLE_DIR="$WORK_DIR/bundle"
mkdir -p "$BUNDLE_DIR/bin" "$BUNDLE_DIR/docs"
printf 'documentation\n' >"$BUNDLE_DIR/docs/README"
regenerate_checksums() {
  (cd "$BUNDLE_DIR" && find . -type f ! -name SHA256SUMS -print0 | sort -z \
    | xargs -0 sha256sum >SHA256SUMS)
}
if (( FAIL_FIXTURE )); then
  cp "$WORK_DIR/fail" "$BUNDLE_DIR/bin/o3kd"
  regenerate_checksums
  if bash "$ROOT_DIR/packaging/verify-release-bundle.sh" "$BUNDLE_DIR"; then
    echo "bundle verifier accepted a binary above the glibc floor" >&2
    exit 1
  fi
fi
cp "$WORK_DIR/pass" "$BUNDLE_DIR/bin/o3kd"
regenerate_checksums
bash "$ROOT_DIR/packaging/verify-release-bundle.sh" "$BUNDLE_DIR"

echo "release glibc baseline tests passed"
