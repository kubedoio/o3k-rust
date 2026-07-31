# Product Requirements — TestLab Alpha (`v0.2.0-alpha.1`)

## Required user journey

Given a supported Linux host with QEMU/KVM and libvirt, a user can:

1. install and start O3K;
2. retrieve generated admin credentials safely;
3. run `openstack token issue`;
4. create/list/show/delete image metadata and upload image content;
5. create/list/show/delete a flat network and subnet;
6. create/list/show/delete a flavor;
7. create/list/show/start/stop/reboot/delete a server;
8. observe the operation and final resource state;
9. retrieve the guest console log through the Nova-compatible operation;
10. restart `o3kd`, `o3k-compute`, and libvirt without losing or duplicating
    the instance;
11. delete the instance and clean all O3K-owned resources;
12. run the complete workflow using documented install, test, reset, and
    uninstall commands.

## Functional requirements

### Identity

- Keystone v3 password authentication for the bootstrap profile;
- project-scoped tokens;
- minimal service catalog;
- explicit token expiry;
- no token or password logging.

### Images

- metadata create/list/show/delete;
- content upload/download for local backend;
- checksum and size recording;
- immutable image content after activation in the alpha profile.

### Networking

- flat provider network;
- one subnet and allocation pool;
- port allocation for server create;
- deterministic cleanup after server delete.

### Compute

- flavors;
- server lifecycle through the versioned provider boundary;
- libvirt/KVM through an `o3k-compute` agent as the primary real backend;
- fake provider for fast contract and failure-path tests;
- CellHV provider as an optional later backend, independently releasable;
- persisted desired and observed states;
- idempotent create/delete handling.

### Operations

- operation ID for every mutation;
- structured state and error information;
- reconciliation after restart;
- bounded retries and visible terminal failure.

## `v0.2.0-alpha.1` exit criteria

The alpha release is release-ready only when executable tests and evidence show
that the required user journey works end to end with a real QEMU/KVM guest
through local libvirt at `qemu:///system`. The evidence must include:

- clean supported Linux installation and documented reproduction commands;
- image verification, qcow2 overlay creation, config-drive/cloud-init data,
  deterministic DHCP fixed-IP delivery, and retrievable boot console output;
- inspect/list/stop/start/reboot/delete lifecycle behavior;
- restart and failure-injection coverage for `o3kd`, `o3k-compute`, libvirt,
  duplicate delivery, timeouts, and unknown outcomes;
- proof that deletion leaves no O3K-owned domain, overlay, config-drive, TAP,
  DHCP binding, Neutron port ownership, Placement allocation, or unfinished
  operation;
- compatibility status for each supported OpenStack operation;
- published footprint/latency measurements, SBOM, checksums, provenance, and
  signed release artifacts.

CellHV support is documented and tested independently where available, but
CellHV is not a prerequisite or substitute for the libvirt alpha gate.

## Non-functional requirements

- clean-host install documented and tested;
- zero external message queue in the alpha profile;
- SQLite default; PostgreSQL compatibility planned;
- structured JSON logs;
- Prometheus metrics and trace correlation IDs;
- `/healthz` and `/readyz`;
- no secret values in metrics, traces, logs, or errors;
- signed release and SBOM before public alpha;
- p95 API latency and resource footprint measured, not guessed.

## Compatibility policy

Each supported OpenStack operation is listed in a compatibility matrix with:

- public reference;
- request and response contract;
- supported fields and microversion;
- known deviations;
- executable test evidence.

Unsupported fields or extensions must not be silently advertised.
