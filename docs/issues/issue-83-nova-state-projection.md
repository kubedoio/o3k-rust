# Issue #83 — Nova powered-off state projection

The portable issue #83 slice corrects the Nova-facing projection of an
observed libvirt `shutdown` or `shutoff` domain. The provider keeps its
internal `Stopped` state, while the reconciler and compute API expose Nova's
`SHUTOFF` status and accept it for start/reboot validation.

This does not claim agent-backed lifecycle dispatch, real guest execution,
restart recovery, or real-host acceptance. Those remain blocked requirements
for issue #83.

The compute-agent command result now carries an explicit provider resource
state, and successful fake/libvirt command paths populate observations with
that state rather than leaving the protobuf field unspecified. This is a
protocol-to-agent evidence slice only; it does not prove a real domain
observation.
