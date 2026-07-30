# SPEC-0001 — TestLab Alpha Vertical Slice

Status: Draft

## Objective

Deliver one complete OpenStack-compatible workflow on a single Linux host using SQLite and a provider abstraction.

## Supported workflow

- project-scoped password authentication;
- image metadata and local content;
- flat network and subnet;
- flavor;
- server create/list/show/start/stop/reboot/delete;
- restart and reconciliation;
- reset/destroy.

## Acceptance criteria

- standard OpenStack CLI can execute the documented workflow;
- every mutation has an operation ID and persisted desired state;
- duplicate create request does not create duplicate provider resources;
- process restart at defined failure points converges or exposes a terminal error;
- unsupported fields/extensions are documented;
- stub and CellHV providers implement the same provider contract;
- E2E test produces logs and compatibility evidence.

## Non-goals

Cinder, live migration, advanced Neutron, multi-node consensus, complete Keystone, complete Nova, and Horizon parity.
