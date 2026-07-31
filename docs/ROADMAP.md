# Roadmap

## Phase 0 — repository bootstrap

- charter, architecture, clean implementation, agent rules;
- Rust workspace and CI;
- health/readiness endpoint;
- initial domain state machine;
- public contract skeletons;
- issue backlog.

## Phase 1 — TestLab contract and fake vertical slice

- SQLite store and migrations;
- Keystone-compatible bootstrap token flow;
- Glance metadata and local content;
- flat network resource model;
- flavor and server APIs;
- stateful fake providers;
- OpenStack CLI smoke workflow;
- restart and reconciliation tests.

## Phase 2 — Libvirt/KVM TestLab vertical slice (`v0.2.0-alpha.1`)

- `o3k-compute` agent and versioned provider protocol;
- secure compute-host registration and heartbeat;
- local `qemu:///system` libvirt/KVM capability discovery;
- image cache, qcow2 overlays, and deterministic domain ownership;
- Placement inventory, scheduling, and allocation tracking;
- Linux bridge, TAP, flat networking, and deterministic DHCP;
- config-drive/cloud-init bootstrap;
- server create/show/start/stop/reboot/delete and console-log retrieval;
- timeout/unknown-outcome recovery and restart discovery;
- clean-host real-libvirt integration and OpenStack CLI E2E evidence;
- packaging, footprint/latency measurements, SBOM, provenance, checksums, and
  signed release artifacts.

The `v0.2.0-alpha.1` exit gate requires the full documented workflow to boot a
real guest through libvirt/KVM, deliver its fixed IP and metadata, survive
`o3kd`, `o3k-compute`, and libvirt restarts, retrieve console output, and clean
up all O3K-owned resources. A skipped integration environment is not passing
evidence.

## Phase 3 — Optional CellHV provider and reproducible follow-on profiles

- volume contract and volume-backed boot work, after the alpha release gate and
  before any later CellHV expansion (not part of `v0.2.0-alpha.1`);
- CellHV capability discovery and provider conformance;
- CellHV compute/network/storage providers;
- independently releasable CellHV integration profiles;
- broader provider and deployment experiments after the libvirt release gate.

## Later, only after evidence

- Cinder subset;
- richer Neutron behavior;
- quotas and policy engine;
- small-cluster coordination;
- edge lifecycle and offline operation;
- production-readiness work for supported SMB profiles.
