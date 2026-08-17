# P7: Mutating Work Source Inventory Matrix

This matrix inventories every path in `o3kd` that causes a durable or external mutation, analyzing multi-controller safety under $\ge 2$ replicas and specifying the required ownership and fencing primitives.

---

## 1. Public Request Paths (Northbound API)

| Work Type | Trigger | Durable Identity | Current Idempotency Key | Current Owner | Provider Side Effect? | Safe Under 2 Controllers Today? | Required Fencing |
|---|---|---|---|---|---|---|---|
| **Server Create** | `POST /v2.1/servers` | `ServerId` (`Uuid`) + `OperationId` | `client_request_token` / `ServerId` | Single controller executing the request | Yes (creates domain, disks, TAP, DHCP, iso) | **No** (concurrent requests with same name/token or different controllers could create duplicate reservations/domains) | `operation:<operation_id>` lease + fencing token on command dispatch and observation |
| **Server Lifecycle (Start/Stop/Reboot/Pause/Resume)** | `POST /v2.1/servers/{id}/action` | `ServerId` + `OperationId` | `OperationId` | Single controller executing the request | Yes (changes domain power state) | **No** (conflicting lifecycle actions from 2 controllers could interleave) | `operation:<operation_id>` / `server:<server_id>` lease + fencing token |
| **Server Delete** | `DELETE /v2.1/servers/{id}` | `ServerId` + `OperationId` | `ServerId` | Single controller executing the request | Yes (destroys libvirt domain, disk overlays, TAP, DHCP) | **No** (duplicate teardown / race with create or lifecycle) | `operation:<operation_id>` lease + fencing token on command dispatch and terminal persistence |
| **Image Create / Upload / Delete** | `POST /v2/images`, `PUT /v2/images/{id}/file`, `DELETE /v2/images/{id}` | `ImageId` (`Uuid`) | `ImageId` | Single controller executing the request | Local filesystem mutation only (store / PVC) | **Yes** (if backed by shared PVC + PostgreSQL row locking) | Shared PostgreSQL row locking |
| **Network / Subnet / Port Create / Delete** | `POST/DELETE /v2.0/networks`, `subnets`, `ports` | `NetworkId`, `SubnetId`, `PortId` (`Uuid`) | Resource `Uuid` | Single controller executing the request | No (metadata in DB until compute binding) | **Yes** (PostgreSQL transactional uniqueness & ACID) | PostgreSQL transactional locking |
| **Placement Allocations** | `PUT/DELETE /allocations/{consumer_uuid}` | `ConsumerId` (`Uuid`) | `ConsumerId` | Single controller | Placement DB allocations | **Yes** (PostgreSQL transaction isolation & generation check) | PostgreSQL optimistic locking / generation checks |
| **Quota Reservations & Commit** | Server create / delete quota checks | `ReservationId` / `ProjectId` | `OperationId` / `ReservationId` | Single controller | Quota usage rows in DB | **Yes** (PostgreSQL transactional reservations) | Transactional quota reservation fencing |
| **Volume Attachment (Cinder)** | `POST/DELETE /v2.1/servers/{id}/os-volume_attachments` | `AttachmentId` (`Uuid`) + `OperationId` | `AttachmentId` | Single controller | Yes (outbound Cinder API calls + guest device attachment) | **No** (duplicate outbound Cinder attach calls) | `attachment:<attachment_id>` / `operation:<operation_id>` lease + fence |

---

## 2. Background Tasks (Reconciliation & Housekeeping)

| Work Type | Trigger | Durable Identity | Current Idempotency Key | Current Owner | Provider Side Effect? | Safe Under 2 Controllers Today? | Required Fencing |
|---|---|---|---|---|---|---|---|
| **Create Convergence Reconciler** | Periodic timer (e.g. 5s) | `OperationId` / `ServerId` | `OperationId` | All controllers run identical background task | Yes (re-drives pending / unobserved creates to compute-agent) | **No** (2 controllers will both re-drive unobserved creates simultaneously, risking duplicate dispatch) | Partitioned `operation:<operation_id>` lease with fencing token |
| **Lifecycle Convergence Reconciler** | Periodic timer (e.g. 5s) | `OperationId` / `ServerId` | `OperationId` | All controllers run identical background task | Yes (re-drives lifecycle actions) | **No** (simultaneous re-drive from multiple replicas) | Partitioned `operation:<operation_id>` lease with fencing token |
| **Attachment Reconciler** | Periodic timer (e.g. 5s) | `AttachmentId` / `OperationId` | `AttachmentId` | All controllers run identical background task | Yes (polls Cinder / re-drives attachment commands) | **No** (duplicate Cinder polling and agent commands) | `attachment:<attachment_id>` lease with fencing token |
| **Agent Inventory Publisher** | Agent registration / periodic notify | `AgentId` | `AgentId` | Every controller with compute agent registry | Updates Placement inventory | **No** (multiple controllers publishing same agent inventory concurrently) | `agent:<agent_id>` stream owner lease |
| **Console Event Consumer** | Agent event stream | `ServerId` + `EventSequence` | `EventSequence` / `ServerId` | Controller receiving agent event stream | Appends to console log buffer / store | **Yes** (if idempotent append) | Agent stream owner only |
| **Placement Orphan Reconciler** | Startup / Periodic | Global | N/A | Startup singleton | Deletes orphaned allocations | **No** (multiple controllers running cleanup concurrently) | Singleton `reconciler:placement_orphans` lease |
| **Artifact-Transfer Recovery** | On agent reconnect / create re-drive | `ArtifactId` | `ArtifactId` | Stream-owning controller | Initiates HTTP artifact stream | **No** (competing artifact streams) | `agent:<agent_id>` stream owner lease |

---

## 3. Agent Communication & Command Dispatch Paths

| Work Type | Trigger | Durable Identity | Current Idempotency Key | Current Owner | Provider Side Effect? | Safe Under 2 Controllers Today? | Required Fencing |
|---|---|---|---|---|---|---|---|
| **Compute Agent Registration** | gRPC `RegisterNode` | `AgentId` + `AgentEpoch` | `AgentEpoch` | Controller accepting gRPC connection | In-memory registry + Placement update | **No** (agent reconnecting to Controller B while Controller A thinks it owns stream) | `agent:<agent_id>` stream lease in PostgreSQL with `fencing_token` |
| **Agent Heartbeat / Health** | gRPC `Heartbeat` | `AgentId` + `AgentEpoch` | `Timestamp` | Stream-connected controller | In-memory node status | **No** (split-brain heartbeats) | Lease renewal tied to active stream ownership |
| **Agent Stream Ownership** | Persistent gRPC connection | `AgentId` + `ControllerId` | `StreamId` | In-memory `NodeRegistry` | Determines which controller can send commands | **No** (each controller has its own independent `NodeRegistry` in memory) | Durable stream lease in `controller_agent_streams` table |
| **Command Dispatch** | API or Reconciler triggering provider | `CommandId` (`Uuid`) | `CommandId` | In-memory stream on local controller | Yes (dispatches gRPC message to agent) | **No** (if API lands on Controller A but agent is connected to Controller B, dispatch fails; if both dispatch, duplicate execution) | Durable pending command queue in PostgreSQL + stream owner claims and dispatches with fence |
| **Command Replay / Re-drive** | Crash recovery / reconnect | `CommandId` | `CommandId` | Stream-owning controller | Yes (re-sends command to agent) | **No** (both controllers could replay same command) | Replay gated by durable command status and stream lease fencing token |
| **Observation Ingestion** | gRPC `ObservationStream` | `ResourceId` + `ObservationSequence` | `ObservationSequence` | Stream-connected controller | Updates durable resource state & operation journal | **No** (stale controller could write outdated observation) | Observation update fenced by generation / fencing token |

---

## 4. Fundamental Invariants Required for P7

1. **Process & Session Authority**:
   Every `o3kd` process generates a stable `ControllerId` (UUID) and monotonically distinct `ControllerEpoch` (UUID / timestamp). It registers and heartbeats in `controller_sessions`.

2. **Durable Work Leases with Monotonic Fencing**:
   Table `work_leases` manages leases keyed by `work_key` (`VARCHAR(255)`).
   Each acquisition or takeover atomically increments `fencing_token` (`BIGINT`).
   Lease renewal and release are fenced by `(controller_id, controller_epoch, fencing_token)`.

3. **Durable Command Routing & Handoff**:
   - API controllers persist `AgentCommandRecord` in `agent_commands` with state `Pending`.
   - The controller holding the active `agent:<agent_id>` stream lease claims pending commands, attaches its current `fencing_token`, and dispatches them over its local gRPC stream.
   - If the agent-owning controller dies, another controller takes over the agent stream lease (incrementing the fence) and drains pending commands.

4. **Fail-Closed on Database Partition**:
   If a controller fails to renew its session or work lease within the lease window, it immediately halts dispatching new commands and rejects local work until re-acquired.
