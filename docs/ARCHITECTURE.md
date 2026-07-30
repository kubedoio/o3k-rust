# Architecture

## Architectural style

O3K Rust is a modular monolith at the HTTP/control-plane layer with explicit provider boundaries. It may be deployed as one process initially, while crate and contract boundaries preserve the option to split components later.

## Layers

```text
Protocol adapters
  OpenStack HTTP, metadata HTTP, operator API
        |
Application services
  commands, queries, authorization, orchestration
        |
Domain
  resource state, invariants, operation state machines
        |
Ports
  store, clock, identity signer, compute/network/storage providers
        |
Adapters
  SQLite/PostgreSQL, stub, CellHV, local/S3
```

Dependencies point inward. The domain does not depend on Axum, SQLx, protobuf-generated provider clients, or OpenStack JSON structures.

## Resource ownership

O3K owns:

- OpenStack-facing IDs and representations;
- identity, project, catalog, role, policy, quota, and placement semantics;
- desired resource state;
- operation state and reconciliation decisions;
- API compatibility and error behavior.

Providers own:

- provider-native execution IDs;
- VM runtime actions;
- host networking actions;
- volume/image data-plane actions;
- capability reporting and observed state.

O3K persists the mapping between O3K resource IDs and provider resource IDs.

## Operation pattern

A mutating request should normally:

1. validate authentication, authorization, schema, quota, and state transition;
2. allocate an operation identity and persist desired state;
3. return or continue according to the public API contract;
4. execute the provider command;
5. observe provider state;
6. persist convergence or a retryable/terminal error;
7. emit audit, metric, and trace events.

A provider timeout means the outcome is unknown. Reconciliation must observe before retrying destructive or duplicating operations.

## Initial deployment profile

- one `o3kd` process;
- SQLite with embedded migrations;
- local image content;
- flat network provider;
- stub compute provider, then CellHV;
- in-process reconciler;
- OpenStack-compatible public ports plus health/metrics endpoints.

## Growth path

1. PostgreSQL adapter;
2. S3 image content;
3. CellHV compute/network/storage providers;
4. separate worker/reconciler deployment if measured need exists;
5. small-cluster coordination and fencing only after an ADR and failure model.

## Shared Rust ecosystem with CellHV

O3K and CellHV may align on:

- Tokio, tonic, Axum, Tower, tracing, metrics, serde, UUID conventions;
- protobuf style and compatibility rules;
- error/status mapping conventions;
- mTLS and enrollment primitives where appropriately extracted.

They must not share internal database models or couple release cycles. Shared crates require independent ownership, semantic versioning, and an explicit consumer justification.
