# Product Requirements — TestLab Alpha

## Required user journey

Given a supported Linux host, a user can:

1. install and start O3K;
2. retrieve generated admin credentials safely;
3. run `openstack token issue`;
4. create/list/show/delete image metadata and upload image content;
5. create/list/show/delete a flat network and subnet;
6. create/list/show/delete a flavor;
7. create/list/show/start/stop/reboot/delete a server;
8. observe the operation and final resource state;
9. restart O3K without losing state;
10. run a documented destroy/reset workflow.

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
- server lifecycle through a provider trait;
- stub provider first;
- CellHV provider as the first real provider;
- persisted desired and observed states;
- idempotent create/delete handling.

### Operations

- operation ID for every mutation;
- structured state and error information;
- reconciliation after restart;
- bounded retries and visible terminal failure.

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
