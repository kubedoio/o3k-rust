# Security remediation status after rejected-candidate fixes

This status is bound to the remediation branch tip and must not be
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
capabilities.  Fresh disposable TestLab run `1786400001` completed the public
CirrOS lifecycle and then exercised config-drive publication under an O3KD
SIGKILL: xorriso publication was observed, O3KD restarted ready, and the
server recovered ACTIVE.  The recovered config-drive tree had no temporary
publication residue.  The same run accepted explicit user-data and vendor-data
and read both back from `openstack/latest` (`user_data` 39 bytes and
`vendor_data.json` 36 bytes), with both files mode `0600`.  It also read a
1-TiB sparse console file and a concurrently growing sparse file through the
public console API; the largest response was 26,873 bytes and the API process
remained bounded.  The run's owned OpenStack, libvirt, and network resources
were removed and the stale-state guard reported an empty inventory.
Fresh run `1786401002` then killed the running non-root compute process,
observed its health boundary disappear, restarted it with the same durable
state, observed authenticated command observations replay on reconnect, and
completed another public CirrOS lifecycle with all owned resources cleaned.
Fresh run `1786401003` paused a real create after libvirt define, restarted
libvirtd asynchronously, and issued delete while the create was paused.  The
first delete was rejected with the expected current-operation conflict; after
create completion the retry converged to absence.  A foreign same-name domain
digest was unchanged before/after (`cc24140437ae576bb17dbc16ddc92411e6bf829dbe1ae32825d487671a45141d`) and the stale-state guard was empty.  This is useful
pass-after recovery evidence but did not reproduce the historical accepted
delete command stranded in #575.

ASR-017 is now CLOSED by PR #593 (merged as `dc9598d`), which re-drives a
stale-accepted delete with one deterministic fresh command identity and
observes it to terminal.  Real-host proof on the exact merged source
(runs `local-575`/`local-5752`/`local-5753` on `nkudo-vm1`, evidence under
`target/real-host-workflow-artifacts/pr575-*`):

- `local-575` (current main `2d4ebef`) reproduced the #575 mid-define
  envelope and exposed the `13d1d65` create-adoption regression (the
  placeholder empty-payload inspection row was rejected by the agent, so an
  interrupted create stayed REQUESTED with its domain running); fixed in the
  same PR.
- `local-5753` (final head) executed the exact scenario: create interrupted
  by a mid-define libvirtd restart, adopted to ACTIVE by the fixed presence
  inspection; delete raced against a second restart; the original delete
  command was accepted and its undefine hit `Domain not found` (the #575
  failure) with the observation rejected; the lifecycle sweep minted the
  deterministic fresh command, the agent executed it to Succeeded, the
  re-drive operation row carried the evidence, and the delete API returned
  rc=0.  Final durable state: original command `unknown_outcome` (historical
  record), re-drive command `succeeded`, delete op `succeeded`, server
  absent, 0 libvirt domains, 0 placement allocations, foreign canary digest
  unchanged, controlled cleanup completed with zero owned residue.

| ASR | State | Current proof | Remaining gate |
|---|---|---|---|
| ASR-001 | in-progress | Attachment routes are profile-gated, authenticated, and project-scoped; negative API tests pass. Fresh Flamingo run `1786403012` reached real Cinder, created a real LVM-backed volume, booted a real server, attached through the public Nova API, observed the run-owned iSCSI session and libvirt disk binding, detached, and cleaned all run-owned resources. Evidence: `/var/lib/o3k-cinder-testbed/1786403012/evidence-1786403514`; the hosted profile remains candidate-bound pending isolation and final review | Fresh successful external-Cinder/hosted-profile attachment evidence is now present; complete hosted-profile claim still requires isolation evidence |
| ASR-002 | in-progress | Attachment repository lookups include project/server ownership; cross-project tests pass. Hosted run `1786403012` proves same-project attachment and cleanup, but does not yet provide a fresh two-tenant hosted isolation exercise | Fresh successful hosted-profile isolation evidence |
| ASR-003 | closed | The rejected-candidate failure mode was tenant-controlled qcow2 dependency access; source `acc2f11` run `1786389106` drove absolute `/etc/hostname`, relative escape, nested chain, file-protocol, external-data, and HTTP-protocol variants through the real agent create path. Every create failed before materialization, cleanup passed, no target sentinel hash appeared in variant outputs, no O3K domain/link remained, and the HTTP canary recorded zero requests; evidence: `/var/tmp/o3k-real-host-evidence-1786389106/asr-003-004-evidence.txt` | None for remediation; candidate-bound recertification remains separate |
| ASR-004 | closed | Source `acc2f11` run `1786389106` records all hostile image creates rejected with no `qemu-img` invocation in the compute log, zero post-run helper children, and zero residual owned resources. The installed-host actual-child capability capture records `CapEff=0`, `NoNewPrivs=1`, and no decoded capabilities; bounded timeout/output/resource behavior is covered by the current portable suite and `run_qemu_img` limits; capability artifact: `/tmp/o3k-real-host-evidence-1786388000/dac-evidence.txt` | None for remediation; candidate-bound recertification remains separate |
| ASR-005 | closed | Portable mode/secret tests plus final-source host artifacts `/tmp/o3k-real-host-evidence-1786388000/dac-evidence.txt` and `/tmp/o3k-real-host-evidence-1786388400/config-drive-dac-evidence.txt` observe DB/WAL/SHM, config-drive media, ownership manifests, and temporary publication files at restrictive modes; disposable TestLab run `1786400001` observed publication through an O3KD SIGKILL/restart, recovered the server ACTIVE, and read explicit user-data/vendor-data back from `openstack/latest` with both files mode `0600`; unrelated `ubuntu` reads are denied | None for remediation; candidate-bound recertification remains separate |
| ASR-006 | closed | Admission limits and restart/symlink-safe config-drive cleanup tests pass; disposable TestLab run `1786400001` observed xorriso publication during an O3KD SIGKILL, waited for the restarted daemon to become ready, recovered the server, and found no temporary publication residue | None for remediation; candidate-bound recertification remains separate |
| ASR-007 | closed | `29de383` host run `1786387900` used a non-root `o3k-compute` capability probe and observed the managed console sink as `o3k-compute:kvm` mode `0660`; the complete CirrOS console lifecycle passed; portable foreign-path/special-file and bounded-read tests pass | None for remediation; candidate-bound recertification remains separate |
| ASR-008 | closed | `29de383` host run `1786387900` passed real CirrOS console capture with a 28,985-byte managed sink; disposable TestLab run `1786400001` read a 1-TiB sparse console file and a concurrently growing sparse file through the public API, with eight reads each capped at 26,873 bytes and the final sparse apparent size at 1,099,521,621,536 bytes; portable sparse/special-file bounds also pass | None for remediation; candidate-bound recertification remains separate |
| ASR-009 | closed | Source `57cbe35` artifact `/tmp/o3k-real-host-evidence-1786388000/dac-evidence.txt`: `o3kd` and `o3k-compute` use separate UIDs and private state; control DB/WAL/SHM are `0600`; cross-account reads are denied; `o3kd` cannot access `qemu:///system` while compute can; TLS keys are account-scoped; the qemu-img child has `NoNewPrivs=1` and zero effective capabilities | None for remediation; clean-install recertification remains separate |
| ASR-010 | closed | Source `acc2f11` run `1786389112` paused after define, replaced the expected `o3k-4b226ec4682ce03cae3a` with a foreign same-name domain using UUID `aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa`, and verified identical before/after foreign XML digests. The agent failed closed with `domain ownership verification failed`; it did not undefine or mutate the foreign domain. After preserving the canary evidence, ownership-aware cleanup completed and no O3K domain/link remained; evidence: `/var/tmp/o3k-real-host-evidence-1786389112/asr-010-evidence.txt` | None for remediation; candidate-bound recertification remains separate |
| ASR-011 | closed | Source `acc2f11` run `1786389110` pre-created a foreign same-name bridge `o3k-canary11` in `DOWN` state; the real agent create failed closed with `existing TAP interface is not owned by the requested O3K network`, cleanup passed, and before/after `ip -d link` digests were identical. The complete stable-MAC lifecycle pass is recorded by run `1786387900`; canary evidence: `/var/tmp/o3k-real-host-evidence-1786389110/asr-011-evidence.txt` | None for remediation; candidate-bound recertification remains separate |
| ASR-012 | closed | Source `acc2f11` real-host hostile-image creates in run `1786389106` failed after network/port preparation and each cleanup verified image/keypair/network/subnet/port/flavor absence with zero owned links; the same run records zero O3K domains/links after all failures. Fresh `TMPDIR=/var/tmp bash tests/packaging-safety.sh` also exercised reset, uninstall, purge, foreign state, and ownership-ledger refusal paths; output is preserved in `/var/tmp/o3k-real-host-evidence-1786389106/packaging-safety.log` | None for remediation; candidate-bound recertification remains separate |
| ASR-013 | implemented-portable | dnsmasq cleanup acquires pidfd before identity validation and signals only the stable handle; process tests pass | Fresh Linux pid-reuse stress proof |
| ASR-014 | closed | Agent evidence is bound to command/resource/agent identity; artifact-offer retries tolerate only expiry refreshes while preserving immutable identity; run `987654346` committed both transfers without the prior offer-conflict disconnect. Fresh disposable run `1786401002` killed the running non-root compute process, observed its health endpoint disappear, restarted it with the same durable journal, observed replayed command observations on reconnect, and then passed a complete public CirrOS lifecycle with owned-resource cleanup | None for remediation; candidate-bound recertification remains separate |
| ASR-015 | closed | Final-head source `294940e` on `nkudo-vm1` captured a real hard-reboot command `accepted` under E1, killed the only non-root compute process after execution but before terminal delivery, registered the same `compute-agent` under fresh E2, and converged the original command and operation to `succeeded` through the serialized agent-command projection implementation. One execution, one unchanged domain UUID, public console/stop/start/delete, zero post-delete allocations or recoverable commands, independent leak cleanup, and a byte-identical foreign canary were verified. Same-source final-head tests prove stale E1, a concurrent E1-projection/E2-registration interleaving, and queued replaced-epoch evidence cannot mutate durable truth. Evidence: `docs/evidence/asr-015-reconnect-host-294940e.json`; stale-epoch tests: `docs/evidence/asr-015-stale-epoch-tests-294940e.txt`; fail-before: `docs/evidence/asr-015-reconnect-host-633f8cb.json` | None for remediation; candidate-bound recertification remains separate |
| ASR-016 | implemented-portable | Monotonic observation and concurrent store tests pass | Fresh multi-process concurrency evidence |
| ASR-017 | closed | PR #593 (merged `dc9598d`) re-drives a stale-accepted delete with one deterministic fresh command identity and observes it to terminal. Real-host run `local-5753` on the exact PR head reproduced the full #575 envelope (mid-define libvirtd restart; delete's undefine hit `Domain not found`; observation rejected; stale command) and converged: delete API rc=0, op `succeeded`, server absent, 0 domains, 0 allocations, canary unchanged. The same PR fixed the `13d1d65` create-adoption regression found by run `local-575`. Evidence: `target/real-host-workflow-artifacts/pr575-*` | None for remediation; candidate-bound recertification remains separate |
| ASR-018 | implemented-portable | Placement intent/commit/restart reconciliation tests pass | Fresh crash-failpoint host proof |
| ASR-019 | closed | Source `acc2f11` host run `TMPDIR=/var/tmp bash tests/packaging-safety.sh` passed the symlinked certificate output canary and all installer/uninstaller ownership checks; preserved output: `/var/tmp/o3k-real-host-evidence-1786389106/packaging-safety.log` | None for remediation; candidate-bound recertification remains separate |
| ASR-020 | closed | `tests/human-review-package.sh` passed on source `61f4308`: pending/approved/rejected artifacts validate truthfully, rejected approval booleans are not required, malformed safeguards are rejected, and `--require-approved` rejects non-approved artifacts | None for validator remediation; human approval is intentionally not created here |
| ASR-021 | in-progress | Candidate-binding gates exist | New candidate-bound recertification |
| ASR-022 | in-progress | Installer refuses unsafe reuse of pre-existing control/compute identities | Fresh clean-install identity proof |

The next campaign must rerun the real-host gates, the failure/recovery matrix,
foreign-state verifier, clean Ubuntu/Debian installation tests, benchmark, and
candidate-bound release checks before any approval or tag is considered.
