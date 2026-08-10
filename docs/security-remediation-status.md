# Security remediation status after rejected-candidate fixes

This status is bound to the remediation branch at `0ff3e8f` and must not be
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
recertification is still required.  Final-source host run `1786387900` on
`29de383` passed the complete public CirrOS lifecycle, including console boot
marker, restart/reconciliation, delete, and zero owned-resource residue; the
managed console sink was observed as `o3k-compute:kvm` mode `0660`.
The installed-style DAC/identity artifact for source `57cbe35` is
`/tmp/o3k-real-host-evidence-1786388000/dac-evidence.txt`; it records separate
service identities, cross-account state denial, the intended libvirt/polkit
boundary, and a sandboxed qemu-img child with `NoNewPrivs=1` and zero effective
capabilities.

| ASR | State | Current proof | Remaining gate |
|---|---|---|---|
| ASR-001 | implemented-portable | Attachment routes are profile-gated, authenticated, and project-scoped; negative API tests pass | Fresh external-Cinder/hosted-profile evidence |
| ASR-002 | implemented-portable | Attachment repository lookups include project/server ownership; cross-project tests pass | Fresh hosted-profile evidence |
| ASR-003 | implemented-portable | Backing-chain and external-data rejection occurs before helper invocation; adversarial tests pass | Fresh real `qemu-img` proof |
| ASR-004 | implemented-portable | Helper limits, bounded output, and capability stripping are tested | Fresh installed-host capability/resource proof |
| ASR-005 | in-progress | Portable mode/secret tests plus final-source host artifacts `/tmp/o3k-real-host-evidence-1786388000/dac-evidence.txt` and `/tmp/o3k-real-host-evidence-1786388400/config-drive-dac-evidence.txt` observe DB/WAL/SHM, config-drive media, ownership manifests, and temporary publication files at restrictive modes; unrelated `ubuntu` reads are denied | Fresh SIGKILL publication/restart proof and explicit user-data/vendor-data admission/readback evidence |
| ASR-006 | implemented-portable | Admission limits and restart/symlink-safe config-drive cleanup tests pass | Fresh host kill/restart evidence |
| ASR-007 | closed | `29de383` host run `1786387900` used a non-root `o3k-compute` capability probe and observed the managed console sink as `o3k-compute:kvm` mode `0660`; the complete CirrOS console lifecycle passed; portable foreign-path/special-file and bounded-read tests pass | None for remediation; candidate-bound recertification remains separate |
| ASR-008 | in-progress | `29de383` host run `1786387900` passed real CirrOS console capture with a 28,985-byte managed sink and portable sparse/special-file bounds; the live console API remained bounded | Fresh host sparse/growing-log adversarial evidence |
| ASR-009 | closed | Source `57cbe35` artifact `/tmp/o3k-real-host-evidence-1786388000/dac-evidence.txt`: `o3kd` and `o3k-compute` use separate UIDs and private state; control DB/WAL/SHM are `0600`; cross-account reads are denied; `o3kd` cannot access `qemu:///system` while compute can; TLS keys are account-scoped; the qemu-img child has `NoNewPrivs=1` and zero effective capabilities | None for remediation; clean-install recertification remains separate |
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
| ASR-020 | closed | `tests/human-review-package.sh` passed on source `61f4308`: pending/approved/rejected artifacts validate truthfully, rejected approval booleans are not required, malformed safeguards are rejected, and `--require-approved` rejects non-approved artifacts | None for validator remediation; human approval is intentionally not created here |
| ASR-021 | in-progress | Candidate-binding gates exist | New candidate-bound recertification |
| ASR-022 | in-progress | Installer refuses unsafe reuse of pre-existing control/compute identities | Fresh clean-install identity proof |

The next campaign must rerun the real-host gates, the failure/recovery matrix,
foreign-state verifier, clean Ubuntu/Debian installation tests, benchmark, and
candidate-bound release checks before any approval or tag is considered.
