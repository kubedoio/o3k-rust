# O3K ASR remediation matrix

This matrix is bound to the rejected candidate
952dcf9c4a1ae958996e4ae9444763e5524eddc5 and is the Prompt 26 audit index.
No row is closed by the historical evidence. A row closes only when the
listed portable regression, required real-host evidence, and fresh
candidate-bound evidence are present and independently reviewable.

| ASR | Severity | Workstream / owner | Architectural root cause | Issue | Required regression and real-host proof | Previous evidence stale after fix | Closure rule |
|---|---|---|---|---|---|---|---|
| ASR-001 | high | Authentication / tenant isolation | Attachment routes trust URL project and omit token authentication | #588 | All four methods: no/invalid/wrong-project token and no provider/DB side effect; external-Cinder profile if enabled | attachment, auth, E2E | 401/403/404 contract and cross-tenant negative pass |
| ASR-002 | medium | Authentication / tenant isolation | Global volume lookup reveals another project's attachment | #588 | Cross-project idempotency and timing/error indistinguishability tests | attachment, auth | Ownership-scoped repository lookup passes |
| ASR-003 | high | Image ingestion / sandboxing | Top-level qcow2 format check preserves untrusted backing chains | #79 | Absolute, relative, nested, protocol, external-data and host-sentinel images rejected; valid raw/standalone qcow2 accepted on host | image, E2E, leak | No host bytes or residue; real qemu-img proof |
| ASR-004 | high | Image ingestion / sandboxing | qemu-img inherits ambient network/DAC capabilities and lacks bounds | #79/#589 | Capability, timeout, output, resource-limit and hostile-image tests; host helper capability inspection | image, install, benchmark | qemu-img has only required authority and bounded execution |
| ASR-005 | high | Secret/file/console safety | Default directory/file modes and shared state expose tenant data | #80/#589 | Unrelated Unix user denied DB/WAL, image, user-data, config-drive, journal and TLS reads on Ubuntu/Debian | install, E2E, leak | Exact modes/owners and mutual state denial proven on hosts |
| ASR-006 | medium | Secret/file/console safety | Input bounds occur after persistence; temp roots permit symlinks and abandoned residue | #80 | Oversized fields rejected before DB write; SIGKILL publication reaper; symlink-root and private-temp tests on host | config-drive, E2E, install | Bounded admission and restart cleanup pass |
| ASR-007 | high | Secret/file/console safety | Arbitrary XML absolute paths plus CAP_DAC_READ_SEARCH bypass DAC | #84/#589 | Forged XML, symlink, FIFO, device, replacement and foreign-file tests; helper capability proof on host | console, install, E2E | Managed-root/no-follow regular-file access without broad DAC capability |
| ASR-008 | high | Secret/file/console safety | Console reads load entire unbounded guest file | #84 | Sparse/huge/growing log tests, bounded seek-tail and rotation/cap tests on host | console, E2E, benchmark | Disk, memory and per-request I/O remain bounded |
| ASR-009 | high | Service-account privilege separation | o3kd and compute share UID, state and libvirt polkit authority | #589 | Installed Ubuntu/Debian UID, DAC, qemu:///system, TLS and helper-capability tests | install, image, console, leak | OS boundary and least authority proven on both hosts |
| ASR-010 | high | Destructive host ownership | Rollback undefines by name after ownership proof fails | #590 | Same-name foreign replacement during every rollback branch; no undefine | libvirt, failure, leak | Destructive action fails closed with current ownership proof |
| ASR-011 | high | Destructive host ownership | Uplink/bridge identity relies on weak or stale name/type state | #590/#81 | Loopback, foreign-master, stale-manifest and replacement bridge canaries on host | network, leak, install | Foreign links survive all operations |
| ASR-012 | high | Destructive host ownership | Fixed system paths are not installation-owned; purge discards ledger with active resources | #590 | Foreign unit/rule files and active domain/TAP/DHCP reset/purge suite | install, leak, failure | Unowned files/resources are preserved and active cleanup fails closed |
| ASR-013 | medium | Destructive host ownership | dnsmasq cleanup validates argv then signals a reusable numeric PID | #590 | Same-user argv spoof and PID-reuse/process-identity tests on host | DHCP, leak, install | Race-resistant handle and exact identity required before signal |
| ASR-014 | medium | Durable command/recovery | Authenticated current agent is not bound to durable assigned command | #83/#87 | Foreign-agent-first, command/resource/agent mismatch tests and process/host evidence | recovery, leak | Only assigned command evidence can mutate durable state |
| ASR-015 | medium | Durable command/recovery | Pending command payload retains stale connection epoch | #83/#87 | Crash after persist-before-accept with fresh epoch; safe rebind and old-stream fencing | recovery, E2E | Converges without duplicate execution or strand |
| ASR-016 | medium | Durable command/recovery | Unconditional operation updates and split evidence transactions race | #83/#87 | Terminal monotonic CAS and atomic provider-ref/watermark/state concurrency tests | recovery, benchmark, leak | Terminal state cannot regress and evidence is atomic |
| ASR-017 | high | Durable command/recovery | Stale accepted delete is promoted to Running and never re-observed | #575/#87 | Exact libvirtd-restart-mid-define reproduction and fresh-command safe re-drive on host — DONE: PR #593 (merged `dc9598d`) re-drives the stale delete with one deterministic fresh command identity; real-host run `local-5753` reproduced the full #575 envelope and converged (delete API rc=0, op succeeded, 0 domains, 0 allocations, canary unchanged); evidence `target/real-host-workflow-artifacts/pr575-*` | recovery, E2E, leak | Delete converges with allocation/resource cleanup; #575 linked explicitly; candidate-bound recertification remains separate |
| ASR-018 | medium | Durable command/recovery / Placement | Allocation commits before durable consumer intent; startup does not reconcile | #82 | Crash failpoint and startup orphan reconciliation; subsequent create and idempotency on host | placement, recovery, leak | No orphan allocation after restart |
| ASR-019 | medium | Destructive host ownership | Certificate bootstrap follows symlinked components after weak path checks | #590 | Symlinked output canary during root bootstrap on host | install | Foreign target preserved and write refused |
| ASR-020 | medium | Evidence/governance | Rejected review status still requires approval booleans; safeguards field not validated | #92 | Validator tests for truthful rejected/pending/approved artifacts and --require-approved | human review, release gate | Rejection validates without approval; approved still requires all gates |
| ASR-021 | medium | Evidence/governance | Machine artifacts are bound to ancestor commits rather than final candidate | #92/#93 | Candidate-bound source manifest and fresh full evidence sequence | all ancestor evidence | Every release artifact names exact candidate source/binaries |
| ASR-022 | high | Installer / service identity | Existing `o3k` control account can retain host-execution supplementary groups during libvirt install | #589 | Installer refuses `o3k` reuse when `libvirt` or `kvm` groups are present; clean-install account-boundary proof | prior install evidence | Refusal and clean fresh-install UID/DAC/libvirt proof are candidate-bound |

## Workstream issue map

- #588: ASR-001/002 authentication and tenant isolation.
- #79: ASR-003/004 image ingestion; #589 owns the OS identity/capability part.
- #80: ASR-005/006 config-drive and secret-bearing state; #84 owns console behavior.
- #589: ASR-009 and shared privilege portions of ASR-004/005/007.
- #590: ASR-010/011/012/013/019 destructive host ownership.
- #82: ASR-018 Placement allocation recovery.
- #83/#87/#575: ASR-014/015/016/017 durable command and lifecycle recovery.
- #92/#93: ASR-020/021 evidence and release governance.
- #589: ASR-022 unsafe pre-existing control-account privilege reuse.

No ASR is accepted as a release limitation. Lower-severity rows remain open
until their stated evidence exists.
