# Security remediation status after rejected-candidate fixes

This status is bound to the remediation branch at `0dda944` and must not be
used as release or human-review approval.  Host evidence from the rejected
candidate is stale after these changes.  Disposable real-host runs now prove
bootstrap/readiness, authentication, managed image upload, network/port/server
creation, and cleanup with every created resource verified absent.  Run
`987654340` failed only the console boot-marker assertion; run `987654341`
exposed a separate create-operation conflict, and run `987654343` reproduced a
TAP-ownership/DHCP rollback failure while leaving the shared `o3k-br0` bridge
present.  The disposable bootstrap now uses a validated per-run bridge name;
run `987654344` completed resource cleanup without leaving its run bridge.
Fresh candidate-bound run `987654403` exposed Linux bridge-MAC drift after
TAP preparation: the live bridge no longer matched the identity recorded at
creation, so the agent correctly failed closed.  The network manager now
assigns a stable locally-administered bridge MAC before recording ownership.
Fresh run `987654406` created a guest with that stable identity and completed
owned-resource cleanup; its lifecycle artifact still failed because the
CirrOS console produced no boot marker.  The pre-existing libvirt domain
`fcanary88` remained unchanged.  The stale `o3k-b87654403` bridge was removed
only after ownership-specific link-down and `brctl delbr` instructions.  The
post-run inventory for `987654406` has no O3K domains, links, or OpenStack
resources and retains the foreign domain.  These are diagnostic results, not
a complete passing lifecycle or release approval; clean-boundary
recertification is still required.

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
| ASR-011 | in-progress | Network cleanup revalidates live ownership and preserves foreign replacement links; `5c65db9` gives disposable runs a validated per-run bridge, `3024edb` pins its MAC before TAP attachment, and run `987654406` removed its owned bridge without touching the foreign domain | Fresh host link canaries and a complete lifecycle run after the bridge-isolation fix |
| ASR-012 | in-progress | Reset/purge preserve ledgers and fail closed on active/foreign host state; `9f412e4` corrects the compute-owned network/DHCP ledger path, `02a294f` fences missing-ledger deterministic bridges, and `0547abd` exports custom bridge identity to cleanup | Fresh failed-create cleanup proving owned bridge/DHCP residue is detected and safely resolved |
| ASR-013 | implemented-portable | dnsmasq cleanup acquires pidfd before identity validation and signals only the stable handle; process tests pass | Fresh Linux pid-reuse stress proof |
| ASR-014 | in-progress | Agent evidence is bound to command/resource/agent identity; artifact-offer retries tolerate only expiry refreshes while preserving immutable identity; run `987654346` committed both transfers without the prior offer-conflict disconnect | Fresh process/agent reconnect evidence and a complete lifecycle run |
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
