#!/usr/bin/env bash
# Deterministic test for the issue #500 runner diagnostics fix: the protected
# runner's failure diagnostics must query Cinder's real `volume_attachment`
# columns (`attach_status`, redacted connection_info/connector presence flags),
# never the nonexistent `status` column that produced empty evidence
# (protected run local-1786012319).

set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNNER="${ROOT_DIR}/scripts/real-cinder-testbed-runner.sh"

[ -f "${RUNNER}" ] || { echo "runner script missing" >&2; exit 1; }

# The attachment diagnostics query must select attach_status and the presence
# flags, and must not select a bare `status` column from the attachment table.
grep -q 'attach_status, attach_mode, attach_time, detach_time, deleted,' \
  "${RUNNER}" || { echo "runner must select attach_status (not status)" >&2; exit 1; }
grep -q '(connection_info IS NOT NULL), (connector IS NOT NULL)' \
  "${RUNNER}" || { echo "runner must select redacted connection_info/connector presence flags" >&2; exit 1; }

# The volume query may use `status` (a real column on the volumes table), but
# the attachment query must not. Verify the attachment SELECT has no bare
# `status,` token immediately after `attached_host,`.
if grep -q 'attached_host, status, attach_status' "${RUNNER}"; then
  echo "runner must not select a nonexistent status column from volume_attachment" >&2
  exit 1
fi

# The volume query legitimately uses status on the volumes table.
grep -q 'display_name, status, attach_status, host,' "${RUNNER}" \
  || { echo "volume diagnostics must keep the real volumes.status column" >&2; exit 1; }

# The protected runner's OSC must use Cinder volume API 3.27+ for the
# `openstack volume attachment list` verification command (regression: the
# command errored "3.27 or greater is required" and the run aborted at the
# attached-state poll even though the attachment was attached).
grep -q 'OS_VOLUME_API_VERSION="3.44"' "${RUNNER}" \
  || { echo "runner must set OS_VOLUME_API_VERSION for volume attachment list" >&2; exit 1; }

echo "real-cinder attachment diagnostics use attach_status + redacted presence flags"
