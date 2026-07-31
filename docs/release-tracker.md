# v0.2.0-alpha.1 release tracker

This file is the source-controlled status record for issue #54. A merged PR
means the scoped implementation and repository gates passed; it does not mean
the real-libvirt acceptance evidence exists.

| Issue | Scope | Repository state | Real-host evidence |
|---:|---|---|---|
| #36 | libvirt/KVM direction | merged | pending |
| #37 | compute-agent protocol | merged | protocol tests passed |
| #38 | secure registration/heartbeat | merged | host deployment pending |
| #39 | local libvirt adapter | merged | host libvirt pending |
| #40 | domain XML/ownership/discovery | merged | host discovery pending |
| #41 | image cache/overlays | merged | atomic/idempotent publication for base, upload, metadata, and overlays added; host qemu-img evidence pending |
| #42 | config-drive | merged | atomic replacement publication added; guest cloud-init evidence pending |
| #43 | Placement | merged | unique atomic state publication added; integration allocation pending |
| #44 | scheduler | merged | agent-targeted scheduling contract added; integration dispatch pending |
| #45 | bridge/TAP | merged | TAP reuse ownership fencing and unique network metadata publication added; privileged host network evidence pending |
| #46 | DHCP/fixed IP | merged | atomic publication and owned dnsmasq supervision added; TAP/dnsmasq/guest IP evidence pending |
| #47 | libvirt lifecycle backend | merged | durable journal, command router, scheduler binding, canonical create builder, durable agent-event reconciliation, live control-plane event consumer, fake command realization, and typed resolved create inputs added; real artifact realization/create and host evidence pending |
| #48 | console log | merged | atomic writes and bounded offset reads added; guest boot output evidence pending |
| #49 | real-libvirt harness | merged | preflight is skipped here |
| #50 | OpenStack CLI workflow | merged | CLI/libvirt endpoint pending; failure cleanup, list coverage, and redacted resource evidence added |
| #51 | clean-host packaging | merged | Ubuntu/Debian clean installs pending |
| #52 | measurements | merged | fake control-plane measured with configured-password authentication; guest metrics pending |
| #53 | release gate | merged | gate is blocked by the rows above |
| #54 | program tracker | this change | tracked here |

## Current release gate

As of this revision, the host has no `virsh`, `/dev/kvm`, or `openstack`
command. The real-libvirt and CLI scripts therefore emit explicit `skipped`
results. `packaging/release-gate.sh` requires `passed` results for real E2E,
failure recovery, clean Ubuntu install, clean Debian install, and a measured
benchmark before it reports `ready`. No release tag is created while the gate
is blocked.

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
(image publication temporaries), and ADR-0036 (network metadata publication).
Release policy and evidence rules
are in `docs/RELEASE.md`, `docs/compatibility.md`, and the #53 release gate.
