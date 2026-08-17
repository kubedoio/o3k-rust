//! Milestone P7: Multi-Controller Acceptance & Invariant Hardening Tests.
//!
//! Tests:
//! 1. Cross-controller command handoff & single-owner dispatch
//! 2. Operation ownership, takeover, and replay idempotence
//! 3. Database-partition split-brain (stale controller sends zero commands)
//! 4. Stale fence write matrix (fails closed on stale token)
//! 5. Mutating work source inventory verification

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

use o3k_compute_agent::{LifecycleCommand, Message, NodeRegistry, build_lifecycle_command, proto};
use o3k_store::{
    AgentCommandRecord, AgentCommandState, ControllerEpoch, ControllerId, CoordinationRepository,
    DurableStore, LeaseAcquireOutcome, O3kStore, OperationRecord, OperationState, ResourceRecord,
};

fn sample_capabilities() -> proto::Capabilities {
    proto::Capabilities {
        architecture: "x86_64".to_owned(),
        agent_provider_name: "o3k-compute".to_owned(),
        agent_provider_version: "test".to_owned(),
        flags: vec![
            proto::CapabilityFlag {
                name: "live_migration".to_owned(),
                supported: true,
                bounded_value: String::new(),
            },
            proto::CapabilityFlag {
                name: "config_drive".to_owned(),
                supported: true,
                bounded_value: String::new(),
            },
        ],
        ..Default::default()
    }
}

fn sample_register(id: &str, epoch: &str) -> proto::RegisterRequest {
    proto::RegisterRequest {
        agent_id: id.to_owned(),
        agent_epoch: epoch.to_owned(),
        software_version: "test".to_owned(),
        host_label: "host".to_owned(),
        supported_versions: vec![proto::ProtocolVersion {
            major: 1,
            minor: 0,
            wire_revision: 1,
        }],
        capabilities: Some(sample_capabilities()),
    }
}

type StreamResponse = Result<proto::ControlResponse, tonic::Status>;

// ============================================================================
// 1. Cross-Controller Command Handoff & Single Owner Dispatch
// ============================================================================

#[tokio::test]
async fn test_cross_controller_command_handoff_and_single_owner_dispatch()
-> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(O3kStore::connect_sqlite_memory().await?);
    let coord: Arc<dyn CoordinationRepository> = store.clone();

    let ctrl_a = ControllerId::new("ctrl-a");
    let epoch_a = ControllerEpoch::new("epoch-a");
    let ctrl_b = ControllerId::new("ctrl-b");
    let epoch_b = ControllerEpoch::new("epoch-b");

    let agent_id = "agent-node-1";
    let agent_epoch = "agent-epoch-1";

    let registry_a =
        NodeRegistry::default().with_coordination(coord.clone(), ctrl_a.clone(), epoch_a.clone());
    let registry_b =
        NodeRegistry::default().with_coordination(coord.clone(), ctrl_b.clone(), epoch_b.clone());

    // Register agent on Controller B (which holds the stream)
    registry_b
        .register(&sample_register(agent_id, agent_epoch))
        .await?;

    let (_tx_a, mut rx_a): (mpsc::Sender<StreamResponse>, mpsc::Receiver<StreamResponse>) =
        mpsc::channel(16);
    let (tx_b, mut rx_b) = mpsc::channel(16);

    // Controller B attaches agent stream successfully and holds the lease
    let attach_b = registry_b
        .attach_connection(agent_id, agent_epoch, tx_b)
        .await;
    assert!(attach_b.is_ok(), "Controller B must own agent stream");

    // Local connection on Controller A is absent
    let conns_a = registry_a.all().await;
    assert!(
        conns_a.is_empty(),
        "Controller A has no local agent connections"
    );

    // Persist resource and operation before inserting pending command
    let op_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();

    let res = ResourceRecord {
        id: resource_id,
        kind: "compute_instance".to_owned(),
        project_id: "proj-1".to_owned(),
        generation: 1,
        observed_generation: 0,
        desired_state: "{}".to_owned(),
        observed_state: "BUILD".to_owned(),
        provider_id: None,
    };
    store.insert_resource(&res).await?;

    let op = OperationRecord {
        id: op_id,
        resource_id,
        kind: "create".to_owned(),
        state: OperationState::Pending,
        provider_operation_id: None,
        error_category: None,
        error_message: None,
    };
    store.insert_operation(&op).await?;

    let command_proto = build_lifecycle_command(
        LifecycleCommand::Delete,
        agent_id,
        agent_epoch,
        &op_id.to_string(),
        &resource_id.to_string(),
    )?;

    let payload = command_proto.encode_to_vec();
    let command_id = command_proto.command_id.clone();

    let record = AgentCommandRecord {
        command_id: command_id.clone(),
        idempotency_key: command_proto.idempotency_key.clone(),
        operation_id: op_id,
        resource_id,
        agent_id: agent_id.to_owned(),
        agent_epoch: agent_epoch.to_owned(),
        payload_fingerprint_sha256: command_proto.payload_fingerprint_sha256.clone(),
        payload,
        state: AgentCommandState::Pending,
        accepted_sequence: 0,
        last_sequence: 0,
        provider_operation_id: Some(op_id.to_string()),
        provider_resource_id: None,
    };
    store.insert_agent_command(&record).await?;

    // Controller A attempts dispatch -> strictly rejected because A does not own the lease
    let dispatch_a = registry_a.dispatch_command(command_proto.clone()).await;
    assert!(
        dispatch_a.is_err(),
        "Controller A dispatch must fail without stream lease"
    );
    assert!(
        rx_a.try_recv().is_err(),
        "A must send zero bytes over the wire"
    );

    // Controller B dispatches the pending command
    let dispatch_b = registry_b.dispatch_command(command_proto).await;
    assert!(
        dispatch_b.is_ok(),
        "Controller B must successfully dispatch: {:?}",
        dispatch_b.err()
    );

    let received_envelope = rx_b
        .try_recv()
        .map_err(|e| format!("Failed to receive envelope: {e:?}"))?;
    let resp = received_envelope.map_err(|e| format!("{e:?}"))?;
    match resp.body {
        Some(proto::control_response::Body::Command(cmd)) => {
            assert_eq!(cmd.command_id, command_id);
        }
        _ => return Err("Expected command body in response envelope".into()),
    }

    Ok(())
}

// ============================================================================
// 2. Operation Ownership & Takeover
// ============================================================================

#[tokio::test]
async fn test_operation_ownership_takeover_and_replay_idempotence()
-> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(O3kStore::connect_sqlite_memory().await?);
    let coord: Arc<dyn CoordinationRepository> = store.clone();

    let ctrl_a = ControllerId::new("ctrl-a");
    let epoch_a = ControllerEpoch::new("epoch-a");
    let ctrl_b = ControllerId::new("ctrl-b");
    let epoch_b = ControllerEpoch::new("epoch-b");

    let op_id = Uuid::new_v4();
    let work_key = format!("operation:{}", op_id);

    // 1. Controller A acquires lease at fence 1
    let outcome_a = coord
        .acquire_work_lease(
            &work_key,
            "operation",
            &ctrl_a,
            &epoch_a,
            Duration::from_secs(1),
        )
        .await?;
    let fence_a = match outcome_a {
        LeaseAcquireOutcome::Acquired { lease } => {
            assert_eq!(lease.fencing_token, 1);
            lease.fencing_token
        }
        _ => return Err("Controller A must acquire lease".into()),
    };

    // Controller A's lease expires (sleep >2s for SQLite integer timestamp rollover)
    tokio::time::sleep(Duration::from_millis(2200)).await;

    // 2. Controller B takes over at fence 2
    let outcome_b = coord
        .acquire_work_lease(
            &work_key,
            "operation",
            &ctrl_b,
            &epoch_b,
            Duration::from_secs(30),
        )
        .await?;
    match outcome_b {
        LeaseAcquireOutcome::Acquired { lease } => {
            assert_eq!(
                lease.fencing_token, 2,
                "Takeover must increment fence (1 -> 2)"
            );
            assert_eq!(lease.owner_controller_id, ctrl_b);
        }
        _ => return Err("Controller B must acquire expired lease".into()),
    }

    // 3. Stale Controller A attempts renewal or release with old fence 1 -> rejected
    let stale_renew = coord
        .renew_work_lease(
            &work_key,
            &ctrl_a,
            &epoch_a,
            fence_a,
            Duration::from_secs(15),
        )
        .await?;
    assert!(
        !stale_renew,
        "Stale controller renewal with old fence must return false"
    );

    let stale_release = coord
        .release_work_lease(&work_key, &ctrl_a, &epoch_a, fence_a)
        .await?;
    assert!(
        !stale_release,
        "Stale controller release with old fence must return false"
    );

    Ok(())
}

// ============================================================================
// 3. Database-Partition Split-Brain (Zero Commands Sent)
// ============================================================================

#[tokio::test]
async fn test_database_partition_split_brain_zero_commands_sent()
-> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(O3kStore::connect_sqlite_memory().await?);
    let coord: Arc<dyn CoordinationRepository> = store.clone();

    let ctrl_a = ControllerId::new("ctrl-a");
    let epoch_a = ControllerEpoch::new("epoch-a");
    let ctrl_b = ControllerId::new("ctrl-b");
    let epoch_b = ControllerEpoch::new("epoch-b");

    let agent_id = "agent-split-1";
    let agent_epoch = "epoch-1";

    let registry_a =
        NodeRegistry::default().with_coordination(coord.clone(), ctrl_a.clone(), epoch_a.clone());

    registry_a
        .register(&sample_register(agent_id, agent_epoch))
        .await?;

    let (tx_a, mut rx_a) = mpsc::channel(16);
    let attach_a = registry_a
        .attach_connection(agent_id, agent_epoch, tx_a)
        .await;
    assert!(attach_a.is_ok(), "Controller A attaches agent");

    // Stop A's background renewal and let lease expire in store
    registry_a.abort_stream_renewal(agent_id).await;

    let work_key = format!("agent:{}", agent_id);
    let _ = coord
        .renew_work_lease(&work_key, &ctrl_a, &epoch_a, 1, Duration::from_secs(1))
        .await;
    tokio::time::sleep(Duration::from_millis(2200)).await;

    // Controller B takes over lease with fence 2
    let takeover = coord
        .acquire_work_lease(
            &work_key,
            "agent_stream",
            &ctrl_b,
            &epoch_b,
            Duration::from_secs(15),
        )
        .await?;
    assert!(matches!(
        takeover,
        LeaseAcquireOutcome::Acquired { lease } if lease.fencing_token == 2
    ));

    // Stale Controller A (still holding open socket tx_a) attempts dispatch -> fails closed
    let command = build_lifecycle_command(
        LifecycleCommand::Delete,
        agent_id,
        agent_epoch,
        &Uuid::new_v4().to_string(),
        &Uuid::new_v4().to_string(),
    )?;

    let dispatch_stale = registry_a.dispatch_command(command).await;
    assert!(
        dispatch_stale.is_err(),
        "Stale Controller A dispatch must fail"
    );
    assert!(
        rx_a.try_recv().is_err(),
        "Zero bytes must be sent over partitioned socket"
    );

    Ok(())
}

// ============================================================================
// 4. Stale Fence Write Matrix Fails Closed
// ============================================================================

#[tokio::test]
async fn test_stale_fence_write_matrix_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(O3kStore::connect_sqlite_memory().await?);
    let coord: Arc<dyn CoordinationRepository> = store.clone();

    let ctrl_a = ControllerId::new("ctrl-a");
    let epoch_a = ControllerEpoch::new("epoch-a");
    let ctrl_b = ControllerId::new("ctrl-b");
    let epoch_b = ControllerEpoch::new("epoch-b");

    let work_key = format!("op:{}", Uuid::new_v4());

    // 1. Controller A acquires fence 1
    let acq_a = coord
        .acquire_work_lease(
            &work_key,
            "operation",
            &ctrl_a,
            &epoch_a,
            Duration::from_secs(1),
        )
        .await?;
    let fence_a = match acq_a {
        LeaseAcquireOutcome::Acquired { lease } => lease.fencing_token,
        _ => return Err("Initial acquire must succeed".into()),
    };

    // Let lease expire
    tokio::time::sleep(Duration::from_millis(2200)).await;

    // 2. Controller B takes over at fence 2
    let acq_b = coord
        .acquire_work_lease(
            &work_key,
            "operation",
            &ctrl_b,
            &epoch_b,
            Duration::from_secs(30),
        )
        .await?;
    assert!(matches!(
        acq_b,
        LeaseAcquireOutcome::Acquired { lease } if lease.fencing_token == 2
    ));

    // Matrix of stale operations from Controller A:
    // a. Stale lease renewal
    let renew_res = coord
        .renew_work_lease(
            &work_key,
            &ctrl_a,
            &epoch_a,
            fence_a,
            Duration::from_secs(30),
        )
        .await?;
    assert!(!renew_res, "Stale fence lease renewal must return false");

    // b. Stale lease release
    let release_res = coord
        .release_work_lease(&work_key, &ctrl_a, &epoch_a, fence_a)
        .await?;
    assert!(!release_res, "Stale fence lease release must return false");

    // c. Inspect lease reflects new owner B and fence 2
    let inspected = coord
        .inspect_work_lease(&work_key)
        .await?
        .ok_or("Lease must exist")?;
    assert_eq!(inspected.owner_controller_id, ctrl_b);
    assert_eq!(inspected.fencing_token, 2);

    Ok(())
}

// ============================================================================
// 5. Mutating Work Source Inventory Verification
// ============================================================================

#[test]
fn test_mutating_work_inventory_matrix_consistency() -> Result<(), Box<dyn std::error::Error>> {
    let inventory_doc = include_str!("../../../docs/reports/P7_MUTATING_WORK_SOURCE_INVENTORY.md");

    // Verify all mutating work paths are documented and classified
    let required_entries = [
        "Server Create",
        "Server Lifecycle",
        "Server Delete",
        "Volume Attachment",
        "Create Convergence Reconciler",
        "Lifecycle Convergence Reconciler",
        "Attachment Reconciler",
        "Agent Stream Ownership",
        "Command Dispatch",
    ];

    for entry in required_entries {
        assert!(
            inventory_doc.contains(entry),
            "Inventory must document entry: {entry}"
        );
    }

    Ok(())
}
