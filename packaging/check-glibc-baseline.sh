#!/usr/bin/env bash
# packaging/check-glibc-baseline.sh — release build-baseline gate
#
# Verifies that ELF binaries do not require a glibc newer than the release
# baseline (default 2.36, the glibc of Debian 12 bookworm). Release binaries
# are built on the Debian 12 baseline (scripts/build-release-binaries-debian12.sh)
# so the same artifact executes on both advertised targets (Ubuntu 24.04 and
# Debian 12); a binary built on Ubuntu 24.04 requires GLIBC_2.38/2.39 symbols
# (for example __isoc23_sscanf, pidfd_getpid, pidfd_spawnp) and fails at exec
# on bookworm's glibc 2.36 with `version 'GLIBC_2.38' not found`.
#
# Usage: packaging/check-glibc-baseline.sh BINARY...
#        O3K_MAX_GLIBC=2.36 packaging/check-glibc-baseline.sh BINARY...
#
# The floor is parsed from `readelf --version-info` (the Version needs
# section), the same data the dynamic loader enforces at exec. Non-ELF files
# are skipped: the check is also invoked on whole bundle directories by
# packaging/verify-release-bundle.sh, which contains scripts and docs.
#
# Exit status: 0 when every ELF binary's highest required glibc version is at
# most O3K_MAX_GLIBC; 1 when a binary requires a newer glibc (the offending
# version and symbol are named); 2 on usage errors.
set -Eeuo pipefail

MAX_GLIBC="${O3K_MAX_GLIBC:-2.36}"
LIMIT_MAJOR="${MAX_GLIBC%%.*}"
LIMIT_MINOR="${MAX_GLIBC#*.}"
LIMIT_MINOR="${LIMIT_MINOR%%.*}"
if [[ ! "$LIMIT_MAJOR" =~ ^[0-9]+$ || ! "$LIMIT_MINOR" =~ ^[0-9]+$ ]]; then
  echo "O3K_MAX_GLIBC must be a major.minor glibc version, got: $MAX_GLIBC" >&2
  exit 2
fi
(( $# > 0 )) || { echo "usage: check-glibc-baseline.sh BINARY..." >&2; exit 2; }
command -v readelf >/dev/null 2>&1 || {
  echo "readelf (binutils) is required to inspect glibc version requirements" >&2
  exit 2
}

status=0
for binary in "$@"; do
  if [[ ! -e "$binary" ]]; then
    echo "error: no such file: $binary" >&2
    exit 2
  fi
  if [[ ! -f "$binary" ]]; then
    echo "error: not a regular file: $binary" >&2
    exit 2
  fi
  if ! readelf -h "$binary" >/dev/null 2>&1; then
    # Not an ELF file (script, documentation, ...): the glibc floor only
    # applies to the shipped native binaries.
    continue
  fi

  max_major=0
  max_minor=0
  offenders=()
  while IFS= read -r symbol; do
    version="${symbol#GLIBC_}"
    major="${version%%.*}"
    minor="${version#*.}"
    minor="${minor%%.*}"
    if (( 10#$major > 10#$LIMIT_MAJOR )) \
        || { (( 10#$major == 10#$LIMIT_MAJOR )) && (( 10#$minor > 10#$LIMIT_MINOR )); }; then
      offenders+=("$symbol")
    fi
    if (( 10#$major > 10#$max_major )) \
        || { (( 10#$major == 10#$max_major )) && (( 10#$minor > 10#$max_minor )); }; then
      max_major="$major"
      max_minor="$minor"
    fi
  done < <(readelf --version-info "$binary" | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sort -Vu)

  if (( ${#offenders[@]} > 0 )); then
    echo "FAIL: $binary requires glibc $max_major.$max_minor (newer than the release baseline $MAX_GLIBC); offending symbol versions: ${offenders[*]}" >&2
    status=1
  else
    echo "ok: $binary requires glibc at most $max_major.$max_minor (release baseline $MAX_GLIBC)"
  fi
done

if (( status != 0 )); then
  echo "release binaries must not require glibc newer than $MAX_GLIBC (Debian 12 bookworm); rebuild them on the Debian 12 baseline: scripts/build-release-binaries-debian12.sh" >&2
  exit 1
fi
