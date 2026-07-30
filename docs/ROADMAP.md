# Roadmap

## Phase 0 — repository bootstrap

- charter, architecture, clean implementation, agent rules;
- Rust workspace and CI;
- health/readiness endpoint;
- initial domain state machine;
- public contract skeletons;
- issue backlog.

## Phase 1 — TestLab stub vertical slice

- SQLite store and migrations;
- Keystone-compatible bootstrap token flow;
- Glance metadata and local content;
- flat network resource model;
- flavor and server APIs;
- stateful stub providers;
- OpenStack CLI smoke workflow;
- restart and reconciliation tests.

## Phase 2 — CellHV vertical slice

- versioned provider protobuf;
- CellHV capability discovery;
- server create/show/start/stop/reboot/delete;
- network and volume contract design;
- timeout/unknown-outcome recovery;
- Linux E2E environment.

## Phase 3 — reproducible alpha

- installer and reset workflow;
- PostgreSQL adapter;
- S3 image backend;
- compatibility matrix;
- resource and latency benchmarks;
- signed release, SBOM, security review;
- first external TestLab pilot.

## Later, only after evidence

- Cinder subset;
- richer Neutron behavior;
- quotas and policy engine;
- small-cluster coordination;
- edge lifecycle and offline operation;
- production-readiness work for supported SMB profiles.
