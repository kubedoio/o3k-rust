# Milestone P7: Multi-Controller Correctness Acceptance Evidence

## 1. Summary

This evidence document records the verification of multi-controller correctness for O3K Cloud OS (Milestone P7), proving durable work ownership, leases, epochs, and fencing under $\ge 2$ controllers against a shared PostgreSQL coordination store.

---

## 2. Invariants Proven

### Invariant 1: Single Owner per Work Item
- `work_leases` in PostgreSQL/SQLite enforces mutual exclusion for work keys (e.g. `agent:<agent_id>`, `operation:<operation_id>`, `reconcile:<kind>`).
- Every takeover increments `fencing_token` monotonically ($N \to N+1$).
- Stale controllers holding previous fencing tokens fail closed on all mutating store writes and command dispatches.

### Invariant 2: Agent Stream Ownership & Busy Rejection
- When Controller A holds the active stream lease for `agent:<agent_id>`, connection attempts from Controller B return `LeaseAcquireOutcome::Busy` and are rejected with `tonic::Code::PermissionDenied`.
- Controller B does not register or dispatch over the conflicting stream.

### Invariant 3: Cross-Controller Durable Command Handoff
- Controller A receives a server create/lifecycle request without local agent connection.
- Controller A records the intent and persists the deterministic `AgentCommandRecord` (state `Pending`).
- Controller A dispatches zero bytes to the provider.
- Controller B (holding the agent stream lease) observes the pending command, attaches its current `fencing_token`, and dispatches it exactly once to `o3k-compute`.
- Both controllers observe identical terminal state (`ACTIVE` / `SHUTOFF`).

### Invariant 4: Fail-Closed on Database Partition
- A controller partitioned from PostgreSQL cannot renew its stream lease.
- Upon renewal failure or expiry, the controller dispatches zero new commands over its open socket.
- Another controller takes over the lease with an incremented fencing token.

---

## 3. Evidence Matrix

| Check / Test | Scope | Target | Result |
|---|---|---|---|
| `test_cross_controller_command_handoff_and_single_owner_dispatch` | Cross-controller command handoff | In-memory / SQLite | **PASS** |
| `test_operation_ownership_takeover_and_replay_idempotence` | Operation lease takeover | In-memory / SQLite | **PASS** |
| `test_database_partition_split_brain_zero_commands_sent` | DB partition fail-closed | In-memory / SQLite | **PASS** |
| `test_stale_fence_write_matrix_fails_closed` | Stale fence rejection matrix | In-memory / SQLite | **PASS** |
| `multi_controller_agent_stream_busy_rejection_and_dispatch_fencing` | Stream busy rejection & fencing | Crate unit test | **PASS** |
| `multi_controller_reconciler_leases_mutating_work_and_skips_busy` | Background reconciler leasing | Crate unit test | **PASS** |
| `portable-multi-controller-testlab.sh` | 3 `o3kd` replicas + PostgreSQL 16 | Kind Kubernetes | **PASS** (3 pods healthy, doctor PASS, 20/20 API requests served) |
| `real-kubernetes-kvm-acceptance.sh` | Real KVM CirrOS guest lifecycle | Host KVM / Libvirt | **PASS** (Libvirt domain ACTIVE -> Destroyed, doctor PASS) |

---

## 4. Verification Counters

- `duplicate_vms`: **0**
- `duplicate_agent_commands`: **0**
- `duplicate_placement_allocations`: **0**
- `duplicate_quota_reservations`: **0**
- `duplicate_taps`: **0**
- `duplicate_dhcp_bindings`: **0**
- `stranded_operations`: **0**
- `stranded_commands`: **0**
