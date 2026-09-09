#!/usr/bin/env bash
# Hardened package provisioning for GitHub-hosted O3K jobs.
#
# Only Ubuntu sources (and domains explicitly listed in
# O3K_CI_APT_REQUIRED_DOMAINS) are made visible to apt.  This prevents
# preinstalled runner-image repositories from influencing unrelated jobs while
# retaining apt's normal signature and hash verification.
set -euo pipefail

usage() {
  echo "usage: $0 render OUTPUT_DIR | update | install PACKAGE..." >&2
  exit 2
}

[[ $# -ge 1 ]] || usage

apt_root=${O3K_CI_APT_ROOT:-/}
required_domains=${O3K_CI_APT_REQUIRED_DOMAINS:-}
apt_etc="${apt_root%/}/etc/apt"
source_list="${apt_etc}/sources.list"
source_parts="${apt_etc}/sources.list.d"

domain_allowed() {
  local host=$1 domain
  case "$host" in
    archive.ubuntu.com|security.ubuntu.com|ports.ubuntu.com|azure.archive.ubuntu.com|*.ubuntu.com)
      return 0 ;;
  esac
  IFS=',' read -r -a domains <<< "$required_domains"
  for domain in "${domains[@]}"; do
    [[ -n "$domain" && ( "$host" == "$domain" || "$host" == *".$domain" ) ]] && return 0
  done
  return 1
}

source_allowed() {
  local file=$1 url host
  mapfile -t urls < <(grep -Eo 'https?://[^[:space:]">]+' "$file" || true)
  ((${#urls[@]} > 0)) || return 1
  for url in "${urls[@]}"; do
    host=${url#*://}; host=${host%%/*}; host=${host%%:*}
    domain_allowed "$host" || return 1
  done
}

render_sources() {
  local output=$1 file base destination
  mkdir -p "$output/parts"
  : > "$output/sources.list"
  for file in "$source_list" "$source_parts"/*.list "$source_parts"/*.sources; do
    [[ -f "$file" ]] || continue
    source_allowed "$file" || continue
    base=$(basename "$file")
    # Normalize the runner's regional Ubuntu mirror without changing trust
    # metadata or disabling apt integrity checks.
    if [[ "$file" == *.sources ]]; then
      destination="$output/parts/$base"
      sed -e 's|mirror+file:/etc/apt/apt-mirrors.txt|https://archive.ubuntu.com/ubuntu|g' \
          -e 's|azure.archive.ubuntu.com|archive.ubuntu.com|g' "$file" > "$destination"
    else
      destination="$output/sources.list"
      sed -e 's|mirror+file:/etc/apt/apt-mirrors.txt|https://archive.ubuntu.com/ubuntu|g' \
          -e 's|azure.archive.ubuntu.com|archive.ubuntu.com|g' "$file" >> "$destination"
      printf '\n' >> "$destination"
    fi
  done
  [[ -s "$output/sources.list" || -n "$(find "$output/parts" -type f -size +0c -print -quit)" ]] || {
    echo "no trusted Ubuntu/required APT sources found" >&2
    return 1
  }
}

run_apt() {
  local command=$1; shift
  local temp_dir source_file
  temp_dir=$(mktemp -d)
  trap 'rm -rf "${temp_dir:-}"' EXIT
  render_sources "$temp_dir"
  source_file="$temp_dir/sources.list"
  local opts=(
    -o "Dir::Etc::sourcelist=$source_file"
    -o "Dir::Etc::sourceparts=$temp_dir/parts"
    -o Acquire::Retries=5
    -o Acquire::ForceIPv4=true
    -o Acquire::http::Timeout=20
    -o Acquire::https::Timeout=20
    -o DPkg::Lock::Timeout=120
  )
  echo "APT sources retained: $(grep -REho 'https?://[^[:space:]">]+' "$temp_dir" | paste -sd, -)"
  if [[ "$command" == install ]]; then
    timeout --foreground 900s sudo -n env DEBIAN_FRONTEND=noninteractive \
      apt-get "${opts[@]}" update
  fi
  timeout --foreground 900s sudo -n env DEBIAN_FRONTEND=noninteractive \
    apt-get "${opts[@]}" "$command" "$@"
}

case "$1" in
  render) shift; (($# == 1)) || usage; render_sources "$1" ;;
  update) shift; (($# == 0)) || usage; run_apt update ;;
  install) shift; (($# > 0)) || usage; run_apt install --no-install-recommends -y "$@" ;;
  *) usage ;;
esac
