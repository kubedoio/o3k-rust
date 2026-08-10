# Security remediation status after rejected-candidate fixes

This status is bound to the remediation branch at `a713f48` and must not be
used as release or human-review approval.  Host evidence from the rejected
candidate is stale after these changes.  Disposable real-host runs now prove
bootstrap/readiness, authentication, managed image upload, network/port/server
creation, and cleanup with every created resource verified absent.  Run
`987654340` failed only the console boot-marker assertion; run `987654341`
exposed a separate create-operation conflict, and run `987654343` reproduced a
TAP-ownership/DHCP rollback failure while leaving the shared `o3k-br0` bridge
present.  The disposable bootstrap now uses a validated per-run bridge name;
run `987654344` completed resource cleanup without leaving its run bridge.
These artifacts are useful
diagnostics, but neither is a complete passing lifecycle, so no host-bound row
is marked closed here.  The pre-existing libvirt domain `fcanary88` remained
unchanged.

| ASR | State | Current proof | Remaining gate |
|---|---|---|---|
| ASR-001 | implemented-portable | Attachment routes are profile-gated, authenticated, and project-scoped; negative API tests pass | Fresh external-Cinder/hosted-profile evidence |
| ASR-002 | implemented-portable | Attachment repository lookups include project/server ownership; cross-project tests pass | Fresh hosted-profile evidence |
| ASR-003 | implemented-portable | Backing-chain and external-data rejection occurs before helper invocation; adversarial tests pass | Fresh real `qemu-img` proof |
| ASR-004 | implemented-portable | Helper limits, bounded output, and capability stripping are tested | Fresh installed-host capability/resource proof |
| ASR-005 | in-progress | Restrictive modes and separate state roots are packaged; SQLite, WAL, and SHM are explicitly forced to `0600`, verified by unit and fresh-host bootstrap checks; a live run observed all three at `0600` | Fresh Ubuntu/Debian DAC proof |
| ASR-006 | implemented-portable | Admission limits and restart/symlink-safe config-drive cleanup tests pass | Fresh host kill/restart evidence |
| ASR-007 | in-progress | Managed-root regular-file console checks and bounded reads are tested | Fresh installed-host capability and DAC proof |
| ASR-008 | implemented-portable | Console tail reads are bounded by request and snapshot limits | Fresh sparse/growing-log host evidence |
| ASR-009 | in-progress | `o3kd`/`o3k-compute` have separate users, state, units, and polkit authority | Fresh Ubuntu/Debian install proof |
| ASR-010 | implemented-portable | Every agent lifecycle mutation uses ownership-fenced libvirt handles; same-name replacement tests pass | Fresh libvirt failure/replacement proof |
| ASR-011 | in-progress | Network cleanup revalidates live ownership and preserves foreign replacement links; `5c65db9` gives disposable runs a validated per-run bridge, and run `987654344` removed its run bridge cleanly | Fresh host link canaries and a complete lifecycle run after the bridge-isolation fix |
| ASR-012 | implemented-portable | Reset/purge preserve ledgers and fail closed on active/foreign host state | Fresh install/uninstall/purge host suite |
| ASR-013 | implemented-portable | dnsmasq cleanup acquires pidfd before identity validation and signals only the stable handle; process tests pass | Fresh Linux pid-reuse stress proof |
| ASR-014 | implemented-portable | Agent evidence is bound to command/resource/agent identity; artifact-offer retries tolerate only expiry refreshes while preserving immutable identity | Fresh process/agent reconnect evidence |
| ASR-015 | implemented-portable | Epoch fencing and durable command replay tests pass | Fresh reconnect/crash host evidence |
| ASR-016 | implemented-portable | Monotonic observation and concurrent store tests pass | Fresh multi-process concurrency evidence |
| ASR-017 | implemented-portable | Stale-accepted delete now receives a deterministic fresh redrive; regression test passes | Fresh libvirtd-restart-mid-define proof |
| ASR-018 | implemented-portable | Placement intent/commit/restart reconciliation tests pass | Fresh crash-failpoint host proof |
| ASR-019 | in-progress | Bootstrap/install path checks and ownership markers are fenced | Fresh symlink-component host canary |
| ASR-020 | implemented-portable | Rejected artifacts validate with false approvals and `--require-approved` still fails | Independent governance review |
| ASR-021 | in-progress | Candidate-binding gates exist | New candidate-bound recertification |
| ASR-022 | in-progress | Installer refuses unsafe reuse of pre-existing control/compute identities | Fresh clean-install identity proof |

The next campaign must rerun the real-host gates, the failure/recovery matrix,
foreign-state verifier, clean Ubuntu/Debian installation tests, benchmark, and
candidate-bound release checks before any approval or tag is considered.
