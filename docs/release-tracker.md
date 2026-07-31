# v0.2.0-alpha.1 release tracker

<!-- tracker-contract
owner_issue: 94
release_issue: 93
program_status: blocked
closure_decision: pending
-->

This file is the source-controlled status record for issue #54. A merged PR
means the scoped implementation and repository gates passed; it does not mean
the real-libvirt acceptance evidence exists. The tracker contract is checked
by `packaging/validate-program-tracker.sh`; it must remain blocked and pending
until the independent evidence and decisions listed below exist.

| Issue | Scope | Repository state | Real-host evidence |
|---:|---|---|---|
| #36 | libvirt/KVM direction | merged | pending |
| #37 | compute-agent protocol | merged | accepted contract; protocol tests passed; runtime and host security evidence remain pending |
| #38 | secure registration/heartbeat | merged | durable validated agent administrative state, registration-before-heartbeat sequencing, and desired-state acknowledgements added; host deployment and process-level restart evidence pending |
| #39 | local libvirt adapter | merged | provider-backed readiness and vCPU/KVM capability projection added; host libvirt pending |
| #40 | domain XML/ownership/discovery | merged | deterministic ownership discovery now quarantines all duplicate IDs, rejects unsafe source inputs, and validates ownership metadata before managed-domain listing; host discovery pending |
| #41 | image cache/overlays | merged | atomic publication, cache-hit checksum revalidation, and verified project-scoped artifact resolution added; host qemu-img evidence pending |
| #42 | config-drive | merged | atomic replacement publication, bounded network/vendor payloads, and optional read-only libvirt attachment added; Nova wiring and guest cloud-init evidence pending |
| #43 | Placement | merged | unique atomic state publication, allocation-preserving provider synchronization, refresh and re-registration usage reconciliation added; daemon capability publication and disk-capacity policy pending |
| #44 | scheduler | merged | agent-targeted scheduling and registry eligibility gate added; duplicate-name conflicts now roll back newly reserved Placement allocations; production inventory wiring and integration dispatch pending |
| #45 | bridge/TAP | merged | TAP reuse ownership fencing, existing bridge/uplink validation, ownership-checked deletion, unique network metadata publication, and validated libvirt TAP attachment added; host execution/guest network evidence pending |
| #46 | DHCP/fixed IP | merged | atomic publication, owned dnsmasq supervision, and gateway/binding conflict rejection added; TAP/dnsmasq/guest IP evidence pending |
| #47 | libvirt lifecycle backend | merged | distinct CellHV provider selection, durable journal, command router, scheduler binding, canonical create builder, durable agent-event reconciliation, live control-plane event consumer, fake command realization, typed resolved create inputs, lifecycle ownership fencing, and fail-closed libvirt-provider validation for unsupported network inputs added; full network/TAP, artifact, config-drive, agent-backed create realization, and host evidence remain coupled follow-ups |
| #48 | console log | merged | bounded API reads, fenced agent routing, durable observations, serialized per-instance writes, atomic writes, serial-console XML, and libvirt stream reads added; guest boot evidence pending |
| #49 | real-libvirt harness | merged | prerequisite validation invokes the public OpenStack lifecycle workflow with artifact validation; isolated guest/failure scenarios and trusted-host evidence remain pending |
| #50 | OpenStack CLI workflow | merged | local guest-image upload, waited lifecycle transitions, bounded console polling, failure cleanup, validated show/list identity, verified deletion, and redacted evidence with discarded raw CLI errors; stronger script evidence does not replace a trusted real CLI/libvirt run |
| #51 | clean-host packaging | merged | release bundles now install their bundled `bin/o3kd`/`bin/o3k-compute` binaries without Cargo; uninstall removes the complete O3K helper set while retaining state by default and restricts systemd cleanup to the exact default layout, libvirt profile selects libvirt, and reset stops both control-plane and compute services; Ubuntu/Debian clean installs pending |
| #52 | measurements / release-evidence integrity | merged | fake control-plane measurement now preflights the selected port, verifies launched-PID ownership through readiness/token/RSS checkpoints, emits redacted diagnostics for port/child failures, and binds summary to canonical raw JSON; fake artifacts are explicitly non-release evidence; guest metrics pending |
| #53 | release gate | merged | benchmark and cleanup evidence are required; positive, non-future, seven-day freshness checks now reject stale artifacts; gate remains blocked by real-host rows above |
| #54 | program tracker | this change | tracked here |
| #94 | program closure and provenance tracker | repository contract guarded; ADR-0100 | blocked until every required real-host artifact and independent human decision is recorded; no program closure is claimed |
| #77 | protected real-host validation | repository implementation complete; ADR-0084/0103, protected workflow guards, 14-day redacted artifact retention, and portable tests added | host-gated: protected environment, exact labeled runner, configured TestLab/libvirt host, credentials, image, and a passing manual run remain required |
| #76 | protected runner capability probe | repository implementation complete; ADR-0085/0102, redacted atomically published artifact fenced to workflow run/attempt/source commit, workflow preflight, and portable fake-command tests added | host-gated: dedicated non-root labeled runner must produce `status: passed`; repository work does not claim host acceptance |
| #78 | fail-closed real-libvirt profile safety guard | repository implementation complete; ADR-0086, direct `LibvirtProvider` construction removed from `o3kd`, and deterministic config rejection test added | blocked until a separately scoped agent-backed provider path and real-host evidence exist; no host evidence claimed |
| #79 | image cache and overlay safety boundary | repository safety boundary complete; ADR-0087/0104, regular-file checks, post-create qcow2/backing verification, cleanup, and symlink/outside-target regression coverage added | blocked until Glance/agent-backed image realization and real-host qemu-img evidence exist; no host evidence claimed |
| #80 | config-drive attachment | repository failed-generation cleanup and manifest-integrity validation complete; ADR-0088, ADR-0105, and regression coverage remove unpublished temporary directories and reject altered, symlinked, or unexpected published content | blocked until ISO/VFAT media, libvirt/agent attachment, guest cloud-init evidence, and trusted real-host evidence exist |
| #81 | Neutron/TAP/bridge/DHCP lifecycle | repository link-kind and bounded rollback safety complete; ADR-0089/ADR-0106, deterministic failure-injection coverage, and reverse-order cleanup of O3K-created bridge/TAP resources added | blocked until agent-backed create, real TAP/bridge/libvirt/DHCP orchestration, guest fixed-IP evidence, cleanup/restart evidence, and trusted real-host artifact exist; no host acceptance claimed |
| #82 | Placement scheduling and allocations | repository publication rollback, post-delete release-retry, and pre-journal create-conflict boundaries complete; ADR-0090/0107/0108 and regression coverage restore in-memory state after failed ledger publication, retry release after provider deletion, and avoid allocation on known durable conflicts | blocked until agent-backed provider inventory/create/delete wiring, real guest allocation evidence, restart recovery, and trusted real-host scheduling artifact exist; no host acceptance claimed |
| #83 | libvirt lifecycle and observed Nova state | repository observed-state projection boundary complete; ADR-0091 and regression coverage fail closed for paused, crashed, blocked, suspended, unknown, and inconsistent libvirt observations | blocked until agent-backed lifecycle dispatch, real Nova/guest lifecycle evidence, restart/failure recovery, and trusted real-host artifact exist; no host acceptance claimed |
| #84 | libvirt serial console and Nova console-log | repository console ownership fence complete; ADR-0092 and regression coverage require matching O3K domain metadata before opening a libvirt stream | blocked until actual CirrOS output, bounded restart persistence, cross-project CLI isolation, deletion evidence, and trusted real-host console artifact exist; no host acceptance claimed |
| #86 | complete real CirrOS OpenStack CLI acceptance | repository-owned cleanup now verifies absence for every created resource after public CLI deletion; ADR-0093 and stateful no-op regression coverage added | blocked until a protected runner uploads `real-libvirt-e2e.json` with `status: passed`, including real CirrOS ACTIVE/config-drive/console/restart and leak evidence; no host acceptance claimed |
| #87 | real-host failure injection and unknown-outcome recovery | repository action-recovery boundary complete; ADR-0094, deterministic action timeout injection, fingerprinted duplicate replay, and observation-based lifecycle convergence tests added | host-gated: all required crash, timeout, duplicate, partial-completion, corruption, disk-full, cleanup, and aggregate `failure-recovery.json` scenarios still require the protected self-hosted runner; no host acceptance claimed |
| #88 | real-host resource leak and foreign-state guard | repository race-safe inventory boundary complete; ADR-0095, stable two-read snapshots, atomic redacted inventory publication, foreign-state digests, and `resource-leak-result.json` output added | host-gated: full independent inventory around normal and failure-injection suites, including TAP/DHCP/filesystem/ports/Placement/operations/processes, and a trusted clean-host run remain outstanding; no host acceptance claimed |
| #89 | clean Ubuntu installation and TestLab lifecycle | repository clean-install input validation complete; ADR-0096 and packaging regression coverage reject unsafe paths and incomplete libvirt TLS before filesystem publication | host-gated: clean Ubuntu install, dependency/bootstrap validation, real CirrOS lifecycle, reset/reinstall/uninstall/purge, and trusted leak-free `clean-ubuntu-install.json` remain outstanding; no host acceptance claimed |
| #90 | clean Debian installation and full TestLab lifecycle | repository uninstall precondition ordering complete; ADR-0097 and portable packaging coverage ensure rejected purges do not mutate systemd state | host-gated: clean Debian install, dependency/bootstrap validation, real CirrOS lifecycle, reset/reinstall/uninstall/purge, foreign-state preservation, and trusted leak-free `clean-debian-install.json` remain outstanding; no host acceptance claimed |
| #91 | real libvirt footprint and lifecycle measurements | repository benchmark freshness boundary complete; ADR-0098 and regression coverage require the raw benchmark's timestamp to be fresh and identical to the reviewed summary | host-gated: real CirrOS/libvirt measurements, raw samples, host/kernel/libvirt/QEMU/Rust metadata, and `real-libvirt-benchmark.json` with `status: measured` remain outstanding; no host measurement claimed |
| #92 | independent architecture and security review | repository review package complete; ADR-0099, threat-model checklist, versioned evidence schema, and fail-closed validator added | human-gated: an identified non-LLM reviewer must inspect the exact release commit, record findings/dispositions, approve release-blocking and destructive-cleanup protections, and publish `human-review.json`; no human review or approval claimed |
| #93 | release gate and v0.2.0-alpha.1 publication | repository gate now requires an approved human-review artifact bound to an explicit source commit; ADR-0101 and regression coverage added | blocked: real host evidence, clean-install artifacts, measured benchmark, human approval, signed tag, reproducible published artifacts, and operator verification remain outstanding; no release-ready claim |

## Current release gate

As of this revision, the host has no `virsh`, `/dev/kvm`, or `openstack`
command. The real-libvirt and CLI scripts therefore emit explicit `skipped`
results. `packaging/release-gate.sh` requires `passed` results for real E2E,
failure recovery, clean Ubuntu install, clean Debian install, and a measured
benchmark before it reports `ready`. No release tag is created while the gate
is blocked. It also requires an approved human-review artifact whose
`reviewed_commit` matches the explicit `--source-commit`. This tracker is a
closure record, not a substitute for that gate;
issue #93 owns release-gate execution and publication, while issue #94 remains
pending until the complete decision and evidence record exists.

## Evidence required to close the program

1. Run the real-libvirt preflight and OpenStack CLI workflow on a trusted
   Linux host with QEMU/KVM, libvirt, bridge/TAP permissions, dnsmasq, and a
   CirrOS image.
2. Repeat on clean supported Ubuntu and Debian installations; retain redacted
   machine-readable artifacts.
3. Exercise control-plane, compute-agent, and libvirt restart/failure cases;
   verify no managed artifacts leak.
4. Run the measurement harness, attach raw data and environment metadata, and
   review target failures honestly.
5. Run the release gate, human-review the security/destructive-cleanup
surfaces, then create and verify the signed tag/artifacts.

The exact machine-readable artifact contract is documented in
`docs/release-evidence-schema.md`; a preflight or skipped result is not release
evidence.

## Decision log

The implementation decisions are recorded in ADR-0007 (agent security),
ADR-0008 (libvirt adapter), ADR-0010 (DHCP isolation), ADR-0011 (provider
backends), ADR-0012 (console output), and ADR-0013 (lifecycle safety
boundaries), ADR-0014 (Nova create idempotency), ADR-0015 (agent command dispatch),
and ADR-0016 (durable lifecycle operations), ADR-0017 (agent command router),
and ADR-0018 (scheduler and Placement intent), ADR-0019 (canonical create command),
and ADR-0020 (durable agent-event reconciliation), ADR-0021 (live agent-event consumer),
and ADR-0022 (atomic image overlays), ADR-0023 (fake command realization),
and ADR-0024 (atomic config-drive publication), ADR-0025 (atomic DHCP publication),
and ADR-0026 (TAP reuse ownership fencing), ADR-0027 (agent-targeted scheduling),
and ADR-0028 (bounded console offset reads), ADR-0029 (CLI harness failure
cleanup), ADR-0030 (dnsmasq supervision), ADR-0031 (typed resolved create
inputs), ADR-0032 (measurement authentication input), ADR-0033 (CLI list and
resource evidence), ADR-0034 (Placement atomic publication), and ADR-0035
(image publication temporaries), ADR-0036 (network metadata publication),
ADR-0062 (image-cache hit revalidation), ADR-0063 (reset service cleanup), and
ADR-0064 (required benchmark release gate), and ADR-0065 (DHCP gateway-binding
conflict rejection), ADR-0066 (CellHV provider selection), ADR-0067
(authenticated stream identity binding), ADR-0068 (existing bridge/uplink
validation), ADR-0069 (Placement refresh usage reconciliation), ADR-0070
(bounded config-drive network/vendor data), ADR-0071 (serialized console
writers), ADR-0072 (managed-domain listing ownership validation), and ADR-0073
(duplicate-name scheduler allocation rollback), ADR-0074 (durable agent
administrative state), ADR-0075 (hardened CLI lifecycle evidence), and ADR-0076
(Placement registration usage reconciliation), and ADR-0077 (fail-closed libvirt
create inputs), ADR-0078 (release-bundle installer binaries), and ADR-0079
(release-evidence freshness).
ADR-0080 (measurement process ownership) records the benchmark attribution
boundary, and ADR-0081 records the raw-evidence binding. ADR-0037 through ADR-0061 record the
subsequent provider, console, lifecycle, network, placement, CLI, and
measurement decisions. Release policy and evidence rules
are in `docs/RELEASE.md`, `docs/compatibility.md`, and the #53 release gate.
ADR-0082 records complete, foreign-path-safe uninstall helper cleanup.
ADR-0083 records custom-prefix uninstall systemd cleanup safety.
ADR-0084 records the protected real-host workflow and its fail-closed,
redacted guard contract for issue #77. Repository tests do not substitute for
a trusted host run. ADR-0085 records the read-only runner capability probe and
its honest skipped/failed boundaries for issue #76. ADR-0086 records the
fail-closed rejection of the unimplemented direct libvirt daemon path for
issue #78. ADR-0087 records the regular-file and symlink safety boundary for
the image cache and overlays in issue #79. ADR-0104 records post-create
qcow2/backing verification and cleanup for issue #79. ADR-0088 records cleanup of failed
config-drive publication temporaries for issue #80.
ADR-0089 records the existing-link bridge-kind fence for issue #81; this
portable guard does not substitute for real network or guest evidence.
ADR-0090 records transactional in-memory rollback when Placement ledger
publication fails for issue #82; it does not substitute for real Placement,
agent, or host scheduling evidence. ADR-0091 records the fail-closed
projection of libvirt lifecycle observations for issue #83; it does not
substitute for real libvirt, guest, Nova, or host evidence.
ADR-0107 records retry of a recorded Placement release after provider deletion
when the first publication fails; it does not substitute for agent-backed
Placement wiring, restart recovery, or trusted real-host scheduling evidence.
ADR-0108 records checking durable create conflicts before Placement allocation
and retaining rollback on other pre-journal paths; it does not substitute for
agent-backed Placement wiring, restart recovery, or trusted real-host
scheduling evidence.
ADR-0092 records the ownership fence before libvirt console streams are opened
for issue #84; it does not substitute for actual guest output or host evidence.
ADR-0093 records public-CLI absence verification for every resource owned by
the issue #86 harness; it does not substitute for a real CirrOS or protected
host run. ADR-0094 records observation-based recovery for unknown lifecycle
actions; it does not substitute for issue #87's real-host failure-injection
matrix or aggregate recovery artifact. ADR-0095 records the stable,
redacted inventory and foreign-state digest boundary for issue #88; it does
not substitute for the complete independent host inventory or a trusted
normal/failure-injection run. ADR-0096 records pre-mutation clean-install
path and libvirt TLS validation for issue #89; it does not substitute for a
clean Ubuntu installation, lifecycle run, or release evidence artifact.
ADR-0097 records purge ownership validation before service mutation for issue
#90; it does not substitute for a clean Debian installation, lifecycle run, or
release evidence artifact.
ADR-0098 records freshness validation for both the benchmark summary and its
bound raw measurement artifact for issue #91; it does not substitute for real
libvirt execution or host measurement evidence.
ADR-0099 records the versioned human architecture/security review package and
fail-closed validator for issue #92; it does not identify a reviewer or
substitute automated evidence for independent human approval. ADR-0101 records
the issue #93 release-gate binding to that approved artifact and exact source
commit; it does not provide approval or host evidence.
ADR-0100 records the fail-closed source-controlled tracker contract for issue
#94; it does not validate host artifacts, human identity, signatures, or
release publication.
ADR-0102 records the atomic, workflow-attempt-bound capability artifact fence
for issue #76; it does not provide real runner capability evidence or host
acceptance.
ADR-0103 records the explicit 14-day retention for protected real-host
artifacts; it does not provide a runner, host evidence, or issue closure.
