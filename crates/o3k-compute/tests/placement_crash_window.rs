//! ASR-018 portable crash-window regression tests.
//!
//! The placement allocation is committed before the durable create
//! consumer intent (`begin_create`). A crash in between must not orphan
//! capacity: o3kd startup reconciliation releases the orphan allocation,
//! and the retried create stays idempotent. A live consumer (intent
//! already durable) must never lose its allocation to reconciliation, and
//! concurrent reconciliation/persistence must always resolve to a valid
//! serialized result.
//!
//! The tests seed the exact crash states through the same durable store
//! repositories the daemon uses (the mid-flight crash itself is proven on
//! the real host with the `O3K_TEST_FAULT_PAUSE_AFTER_PLACEMENT_COMMIT_MS`
//! failpoint). The store guard exercised here — a create whose allocation
//! was reconciled away fails closed instead of persisting a consumer
//! without capacity accounting — lives in
//! `SqliteStore::insert_resource_and_operation`, and the in-transaction
//! live-consumer re-check that protects the reverse interleaving lives in
//! `SqliteStore::reconcile_consumers`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use o3k_compute::{ComputeService, ServerId, ServerState};
use o3k_placement::{DISK_GB, Inventory, MEMORY_MB, PlacementLedger, VCPU};
use o3k_provider::FakeComputeProvider;
use o3k_scheduler::Scheduler;
use o3k_store::{
    ComputeRepository, DurableStore, OperationRecord, OperationState, PlacementAllocationRecord,
    PlacementRepository, PlacementResourceRecord, ResourceRecord, StoreError, testkit,
};
use uuid::Uuid;

struct Harness {
    service: ComputeService,
    placement: PlacementLedger,
    store: testkit::TestStore,
    database_path: PathBuf,
    placement_root: PathBuf,
    provider: Arc<FakeComputeProvider>,
}

/// Builds a compute service over a file database with a fake provider and a
/// two-VCPU placement provider (`node-a`), mirroring the o3kd agent-provider
/// composition (store + placement ledger + scheduler + provider).
async fn harness(label: &str) -> Result<Harness, Box<dyn std::error::Error>> {
    let database_path =
        std::env::temp_dir().join(format!("o3k-asr018-{label}-{}.sqlite", Uuid::now_v7()));
    let placement_root =
        std::env::temp_dir().join(format!("o3k-asr018-{label}-pl-{}", Uuid::now_v7()));
    let _ = std::fs::remove_file(&database_path);
    let store = testkit::open_file(&database_path).await?;
    let repository: Arc<dyn PlacementRepository> = Arc::new(store.clone());
    let placement = PlacementLedger::open(&placement_root, repository).await?;
    placement
        .register_provider(
            "node-a",
            BTreeMap::from([
                (
                    VCPU.to_owned(),
                    Inventory {
                        total: 2,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
                (
                    MEMORY_MB.to_owned(),
                    Inventory {
                        total: 2048,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
                (
                    DISK_GB.to_owned(),
                    Inventory {
                        total: 20,
                        reserved: 0,
                        allocation_ratio: 1.0,
                        used: 0,
                    },
                ),
            ]),
        )
        .await?;
    let provider = Arc::new(FakeComputeProvider::new());
    let service = ComputeService::new(Arc::new(store.clone()), provider.clone())
        .with_scheduler(Scheduler::new(placement.clone()));
    Ok(Harness {
        service,
        placement,
        store,
        database_path,
        placement_root,
        provider,
    })
}

/// The o3kd startup consumer set (bins/o3kd/src/main.rs): live
/// `compute_instance` resource ids, sorted and deduplicated.
fn consumer_ids(resources: &[ResourceRecord]) -> Vec<String> {
    let mut ids = resources
        .iter()
        .filter(|resource| resource.observed_state != "DELETED")
        .map(|resource| resource.id.to_string())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn server_id(project: &str, idempotency_key: &str) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("o3k:server:{project}:{idempotency_key}").as_bytes(),
    )
}

fn operation_id(project: &str, idempotency_key: &str) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("o3k:operation:{project}:{idempotency_key}").as_bytes(),
    )
}

/// The o3kd startup sequence (bins/o3kd/src/main.rs:336-350): list the
/// durable compute resources and reconcile every placement consumer against
/// them, before the API listener binds.
async fn startup_reconcile(
    store: &testkit::TestStore,
    placement: &PlacementLedger,
) -> Result<o3k_placement::ReconciliationReport, Box<dyn std::error::Error>> {
    let resources = store.list_resources_by_kind("compute_instance").await?;
    Ok(placement
        .reconcile_consumers(&consumer_ids(&resources))
        .await?)
}

/// Test 1 — the exact ASR-018 crash state: provider registered, capacity
/// available, allocation committed, NO server resource, NO create operation,
/// NO durable consumer. Restart reconciliation releases the orphan exactly
/// once, capacity is restored, and the retried request is idempotent.
#[tokio::test]
async fn crash_after_placement_commit_reconciles_orphan_and_retry_succeeds()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = harness("crash-window").await?;
    let project = "project-a";
    let idempotency_key = "request-crash-window";
    let expected_server = server_id(project, idempotency_key);
    let expected_operation = operation_id(project, idempotency_key);
    let flavor = harness.service.flavors()[0].id;

    // Seed the exact crash-window residue through the durable repositories:
    // the allocation committed by the scheduler for the deterministic
    // `allocation-{server}` identity, with no consumer intent behind it.
    let generation = harness
        .store
        .get_provider("node-a")
        .await?
        .ok_or(StoreError::PlacementProviderNotFound)?
        .generation;
    harness
        .store
        .commit_allocation(
            "node-a",
            generation,
            &PlacementAllocationRecord {
                id: format!("allocation-{expected_server}"),
                provider_id: "node-a".to_owned(),
                consumer_id: expected_server.to_string(),
                resources: vec![PlacementResourceRecord {
                    resource_class: VCPU.to_owned(),
                    amount: 1,
                }],
            },
        )
        .await?;
    let crash_state = harness.placement.provider("node-a").await?;
    assert_eq!(crash_state.allocations.len(), 1);
    assert_eq!(crash_state.inventories[VCPU].used, 1);
    assert!(
        matches!(
            harness.store.get_resource(expected_server).await,
            Err(StoreError::ResourceNotFound)
        ),
        "server resource must not exist inside the crash window"
    );
    assert!(
        matches!(
            harness.store.get_operation(expected_operation).await,
            Err(StoreError::OperationNotFound)
        ),
        "create operation must not exist inside the crash window"
    );

    // Restart: reopen the same durable database and run the o3kd startup
    // reconciliation before serving.
    let restarted_store = testkit::open_file(&harness.database_path).await?;
    let restarted_repository: Arc<dyn PlacementRepository> = Arc::new(restarted_store.clone());
    let restarted_placement =
        PlacementLedger::open(&harness.placement_root, restarted_repository).await?;
    let report = startup_reconcile(&restarted_store, &restarted_placement).await?;
    assert_eq!(report.orphaned_allocations.len(), 1);
    assert_eq!(
        report.orphaned_allocations[0].allocation_id,
        format!("allocation-{expected_server}")
    );
    assert!(report.abandoned_intents.is_empty());

    // Post-restart invariants: orphan released exactly once, capacity
    // restored, generation advanced, no invented server, no dispatch.
    let reconciled = restarted_placement.provider("node-a").await?;
    assert!(reconciled.allocations.is_empty());
    assert_eq!(reconciled.inventories[VCPU].used, 0);
    assert!(
        reconciled.generation > crash_state.generation,
        "reconciliation must advance the provider generation"
    );
    assert_eq!(
        harness.provider.instance_count(),
        0,
        "reconciliation must not dispatch compute work"
    );
    assert!(
        matches!(
            restarted_store.get_resource(expected_server).await,
            Err(StoreError::ResourceNotFound)
        ),
        "reconciliation must not invent a server"
    );

    // Re-running reconciliation is a no-op (idempotent).
    let again = startup_reconcile(&restarted_store, &restarted_placement).await?;
    assert!(again.orphaned_allocations.is_empty());
    assert!(again.abandoned_intents.is_empty());
    assert_eq!(
        restarted_placement.provider("node-a").await?.generation,
        reconciled.generation,
        "repeat reconciliation must not mutate durable state"
    );

    // Retry the same logical request through the public create path.
    let restarted_provider = Arc::new(FakeComputeProvider::new());
    let restarted_service = ComputeService::new(
        Arc::new(restarted_store.clone()) as Arc<dyn ComputeRepository>,
        restarted_provider.clone(),
    )
    .with_scheduler(Scheduler::new(restarted_placement.clone()));
    let server = restarted_service
        .create_server(
            project,
            "crash-window".to_owned(),
            "image-1".to_owned(),
            flavor,
            vec!["network-1".to_owned()],
            idempotency_key.to_owned(),
        )
        .await?;
    assert_eq!(server.id.as_uuid(), expected_server);
    assert_eq!(server.state, ServerState::Active);
    assert_eq!(server.host.as_deref(), Some("node-a"));
    assert_eq!(
        restarted_provider.instance_count(),
        1,
        "exactly one instance for the retried create"
    );
    let final_provider = restarted_placement.provider("node-a").await?;
    assert_eq!(
        final_provider.allocations.len(),
        1,
        "exactly one allocation after the retried create"
    );
    assert_eq!(final_provider.inventories[VCPU].used, 1);

    // Delete releases exactly once; a repeated delete changes nothing.
    restarted_service
        .delete_server(project, ServerId::from_uuid(expected_server))
        .await?;
    let after_delete = restarted_placement.provider("node-a").await?;
    assert!(after_delete.allocations.is_empty());
    assert_eq!(after_delete.inventories[VCPU].used, 0);
    restarted_service
        .delete_server(project, ServerId::from_uuid(expected_server))
        .await?;
    let after_repeat_delete = restarted_placement.provider("node-a").await?;
    assert!(after_repeat_delete.allocations.is_empty());
    assert_eq!(after_repeat_delete.inventories[VCPU].used, 0);

    let _ = std::fs::remove_file(&harness.database_path);
    let _ = std::fs::remove_dir_all(&harness.placement_root);
    Ok(())
}

/// Test 2 — the near-boundary valid state: allocation committed AND durable
/// create intent present, provider outcome unknown. Restart reconciliation
/// must retain the allocation; the unknown create is re-driven without a
/// blind reschedule or duplicate mutation.
#[tokio::test]
async fn restart_reconciliation_retains_live_consumer_allocation()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = harness("live-consumer").await?;
    let project = "project-a";
    let expected_server = Uuid::now_v7();
    let expected_operation = Uuid::now_v7();
    let flavor_id = harness.service.flavors()[0].id.to_string();

    // Seed the crash-after-intent residue: allocation committed, resource
    // durable, create operation Running with no provider identity, and no
    // agent command row (the issue-87 S1 residue with placement).
    let generation = harness
        .store
        .get_provider("node-a")
        .await?
        .ok_or(StoreError::PlacementProviderNotFound)?
        .generation;
    let allocation = PlacementAllocationRecord {
        id: format!("allocation-{expected_server}"),
        provider_id: "node-a".to_owned(),
        consumer_id: expected_server.to_string(),
        resources: vec![PlacementResourceRecord {
            resource_class: VCPU.to_owned(),
            amount: 1,
        }],
    };
    harness
        .store
        .commit_allocation("node-a", generation, &allocation)
        .await?;
    let request = o3k_provider::CreateInstanceRequest {
        operation_id: expected_operation,
        o3k_server_id: expected_server,
        project_id: project.to_owned(),
        name: "live-consumer".to_owned(),
        vcpus: 1,
        memory_mib: 512,
        flavor_id: flavor_id.clone(),
        disk_gib: 10,
        image_id: Some("image-1".to_owned()),
        key_name: None,
        keypair_id: None,
        network_ids: vec!["network-1".to_owned()],
        placement_provider_id: Some("node-a".to_owned()),
        placement_allocation_id: Some(allocation.id.clone()),
        config_drive: None,
        idempotency_key: "request-live-consumer".to_owned(),
    };
    harness
        .store
        .insert_resource_and_operation(
            &ResourceRecord {
                id: expected_server,
                kind: "compute_instance".to_owned(),
                project_id: project.to_owned(),
                generation: 1,
                observed_generation: 0,
                desired_state: serde_json::to_string(&request)?,
                observed_state: "REQUESTED".to_owned(),
                provider_id: None,
            },
            &OperationRecord {
                id: expected_operation,
                resource_id: expected_server,
                kind: "create".to_owned(),
                state: OperationState::Running,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            },
            Some(&allocation.id),
        )
        .await?;
    let pre_crash = harness.placement.provider("node-a").await?;
    assert_eq!(pre_crash.allocations.len(), 1);
    assert_eq!(pre_crash.inventories[VCPU].used, 1);
    assert_eq!(harness.provider.instance_count(), 0);

    // Restart: startup reconciliation with the live consumer set.
    let restarted_store = testkit::open_file(&harness.database_path).await?;
    let restarted_repository: Arc<dyn PlacementRepository> = Arc::new(restarted_store.clone());
    let restarted_placement =
        PlacementLedger::open(&harness.placement_root, restarted_repository).await?;
    let report = startup_reconcile(&restarted_store, &restarted_placement).await?;
    assert!(
        report.orphaned_allocations.is_empty(),
        "a live consumer allocation must be retained, never released"
    );
    let retained = restarted_placement.provider("node-a").await?;
    assert_eq!(retained.allocations.len(), 1);
    assert!(
        retained
            .allocations
            .contains_key(&format!("allocation-{expected_server}"))
    );
    assert_eq!(retained.inventories[VCPU].used, 1);

    // The unknown-outcome create converges through the normal path: one
    // dispatch, same host, no second allocation.
    let restarted_provider = Arc::new(FakeComputeProvider::new());
    let restarted_service = ComputeService::new(
        Arc::new(restarted_store.clone()) as Arc<dyn ComputeRepository>,
        restarted_provider.clone(),
    )
    .with_scheduler(Scheduler::new(restarted_placement.clone()));
    let server = restarted_service
        .show_server(project, ServerId::from_uuid(expected_server))
        .await?;
    assert_eq!(server.state, ServerState::Active);
    assert_eq!(server.host.as_deref(), Some("node-a"));
    assert_eq!(
        restarted_provider.instance_count(),
        1,
        "exactly one instance after restart, no duplicate mutation"
    );
    let converged = restarted_placement.provider("node-a").await?;
    assert_eq!(converged.allocations.len(), 1);
    assert_eq!(converged.inventories[VCPU].used, 1);
    assert!(
        converged
            .allocations
            .contains_key(&format!("allocation-{expected_server}"))
    );

    let _ = std::fs::remove_file(&harness.database_path);
    let _ = std::fs::remove_dir_all(&harness.placement_root);
    Ok(())
}

/// Test 3 — concurrent startup reconciliation vs create persistence over one
/// SQLite file: every interleaving must resolve to a valid serialized result.
#[tokio::test]
async fn concurrent_reconcile_and_persistence_never_lose_allocations()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = harness("race").await?;
    let project = "project-a";
    let flavor = harness.service.flavors()[0].id;

    // Serialization 1 (create then reconcile): the live consumer's
    // allocation is retained.
    harness
        .service
        .create_server(
            project,
            "race-a".to_owned(),
            "image-1".to_owned(),
            flavor,
            vec!["network-1".to_owned()],
            "request-race-a".to_owned(),
        )
        .await?;
    let resources = harness
        .store
        .list_resources_by_kind("compute_instance")
        .await?;
    let report = harness
        .placement
        .reconcile_consumers(&consumer_ids(&resources))
        .await?;
    assert!(report.orphaned_allocations.is_empty());
    assert_eq!(
        harness
            .placement
            .provider("node-a")
            .await?
            .allocations
            .len(),
        1
    );

    // Serialization 2 (reconcile then create): the stale consumer snapshot
    // must not let the create persist a consumer whose allocation was
    // reconciled away — it fails closed instead.
    let server_b = Uuid::now_v7();
    let allocation_b = PlacementAllocationRecord {
        id: format!("allocation-{server_b}"),
        provider_id: "node-a".to_owned(),
        consumer_id: server_b.to_string(),
        resources: vec![PlacementResourceRecord {
            resource_class: VCPU.to_owned(),
            amount: 1,
        }],
    };
    let generation = harness
        .store
        .get_provider("node-a")
        .await?
        .ok_or(StoreError::PlacementProviderNotFound)?
        .generation;
    harness
        .store
        .commit_allocation("node-a", generation, &allocation_b)
        .await?;
    let stale_report = harness
        .placement
        .reconcile_consumers(Vec::<String>::new())
        .await?;
    assert_eq!(stale_report.orphaned_allocations.len(), 1);
    assert_eq!(
        stale_report.orphaned_allocations[0].allocation_id,
        allocation_b.id
    );
    let resource_b = ResourceRecord {
        id: server_b,
        kind: "compute_instance".to_owned(),
        project_id: project.to_owned(),
        generation: 1,
        observed_generation: 0,
        desired_state: "{}".to_owned(),
        observed_state: "REQUESTED".to_owned(),
        provider_id: None,
    };
    let operation_b = OperationRecord {
        id: Uuid::now_v7(),
        resource_id: server_b,
        kind: "create".to_owned(),
        state: OperationState::Pending,
        provider_operation_id: None,
        error_category: None,
        error_message: None,
    };
    let persisted = harness
        .store
        .insert_resource_and_operation(&resource_b, &operation_b, Some(&allocation_b.id))
        .await;
    assert!(
        matches!(persisted, Err(StoreError::PlacementAllocationNotFound)),
        "consumer intent must fail closed when its allocation was reconciled away"
    );
    assert!(
        matches!(
            harness.store.get_resource(server_b).await,
            Err(StoreError::ResourceNotFound)
        ),
        "the failed create must not leave consumer rows"
    );
    assert_eq!(
        harness.placement.provider("node-a").await?.inventories[VCPU].used,
        1
    );

    // Serialization 3 (create first, stale reconcile later): the
    // in-transaction live-consumer re-check retains the allocation even when
    // the caller's snapshot is stale.
    let server_c = Uuid::now_v7();
    let allocation_c = PlacementAllocationRecord {
        id: format!("allocation-{server_c}"),
        provider_id: "node-a".to_owned(),
        consumer_id: server_c.to_string(),
        resources: vec![PlacementResourceRecord {
            resource_class: VCPU.to_owned(),
            amount: 1,
        }],
    };
    let generation = harness
        .store
        .get_provider("node-a")
        .await?
        .ok_or(StoreError::PlacementProviderNotFound)?
        .generation;
    harness
        .store
        .commit_allocation("node-a", generation, &allocation_c)
        .await?;
    let resource_c = ResourceRecord {
        id: server_c,
        kind: "compute_instance".to_owned(),
        project_id: project.to_owned(),
        generation: 1,
        observed_generation: 0,
        desired_state: "{}".to_owned(),
        observed_state: "REQUESTED".to_owned(),
        provider_id: None,
    };
    let operation_c = OperationRecord {
        id: Uuid::now_v7(),
        resource_id: server_c,
        kind: "create".to_owned(),
        state: OperationState::Pending,
        provider_operation_id: None,
        error_category: None,
        error_message: None,
    };
    harness
        .store
        .insert_resource_and_operation(&resource_c, &operation_c, Some(&allocation_c.id))
        .await?;
    let stale_after = harness
        .placement
        .reconcile_consumers(Vec::<String>::new())
        .await?;
    assert!(
        stale_after.orphaned_allocations.is_empty(),
        "a live consumer must never lose its allocation to a stale snapshot"
    );
    let provider_c = harness.placement.provider("node-a").await?;
    assert_eq!(provider_c.allocations.len(), 2);
    assert_eq!(provider_c.inventories[VCPU].used, 2);

    // Genuine barrier race: two independent store actors over one file DB.
    for iteration in 0..8 {
        let database_path = std::env::temp_dir().join(format!(
            "o3k-asr018-race-{iteration}-{}.sqlite",
            Uuid::now_v7()
        ));
        let placement_root =
            std::env::temp_dir().join(format!("o3k-asr018-race-{iteration}-pl-{}", Uuid::now_v7()));
        let _ = std::fs::remove_file(&database_path);
        let store_first = Arc::new(testkit::open_file(&database_path).await?);
        let store_second = Arc::new(testkit::open_file(&database_path).await?);
        let placement_repository: Arc<dyn PlacementRepository> = store_first.clone();
        let placement = PlacementLedger::open(&placement_root, placement_repository).await?;
        placement
            .register_provider(
                "node-a",
                BTreeMap::from([
                    (
                        VCPU.to_owned(),
                        Inventory {
                            total: 2,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                    (
                        MEMORY_MB.to_owned(),
                        Inventory {
                            total: 2048,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                    (
                        DISK_GB.to_owned(),
                        Inventory {
                            total: 20,
                            reserved: 0,
                            allocation_ratio: 1.0,
                            used: 0,
                        },
                    ),
                ]),
            )
            .await?;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let server_id = Uuid::now_v7();
        let allocation_id = format!("allocation-{server_id}");

        // Actor B: schedule-style commit then consumer persistence. A commit
        // that loses the generation race aborts the create cleanly.
        let b_store = store_first.clone();
        let b_barrier = barrier.clone();
        let b_allocation_id = allocation_id.clone();
        let actor_b = tokio::spawn(async move {
            b_barrier.wait().await;
            let generation = b_store
                .get_provider("node-a")
                .await?
                .ok_or(StoreError::PlacementProviderNotFound)?
                .generation;
            let allocation = PlacementAllocationRecord {
                id: b_allocation_id,
                provider_id: "node-a".to_owned(),
                consumer_id: server_id.to_string(),
                resources: vec![PlacementResourceRecord {
                    resource_class: VCPU.to_owned(),
                    amount: 1,
                }],
            };
            match b_store
                .commit_allocation("node-a", generation, &allocation)
                .await
            {
                Ok(_) => {
                    let resource = ResourceRecord {
                        id: server_id,
                        kind: "compute_instance".to_owned(),
                        project_id: "project-a".to_owned(),
                        generation: 1,
                        observed_generation: 0,
                        desired_state: "{}".to_owned(),
                        observed_state: "REQUESTED".to_owned(),
                        provider_id: None,
                    };
                    let operation = OperationRecord {
                        id: Uuid::now_v7(),
                        resource_id: server_id,
                        kind: "create".to_owned(),
                        state: OperationState::Pending,
                        provider_operation_id: None,
                        error_category: None,
                        error_message: None,
                    };
                    match b_store
                        .insert_resource_and_operation(&resource, &operation, Some(&allocation.id))
                        .await
                    {
                        Ok(()) => Ok(true),
                        Err(StoreError::PlacementAllocationNotFound) => Ok(false),
                        Err(error) => Err(error),
                    }
                }
                Err(
                    StoreError::PlacementStaleGeneration | StoreError::PlacementAllocationConflict,
                ) => Ok(false),
                Err(error) => Err(error),
            }
        });

        // Actor A: startup orphan reconciliation (read the consumer snapshot,
        // then reconcile) — the full o3kd startup sequence.
        let a_store = store_second.clone();
        let a_barrier = barrier;
        let actor_a = tokio::spawn(async move {
            a_barrier.wait().await;
            let resources = a_store.list_resources_by_kind("compute_instance").await?;
            let mut ids = resources
                .iter()
                .filter(|resource| resource.observed_state != "DELETED")
                .map(|resource| resource.id.to_string())
                .collect::<Vec<_>>();
            ids.sort();
            ids.dedup();
            a_store.reconcile_consumers(&ids).await
        });
        let (b_result, a_result) = tokio::join!(actor_b, actor_a);
        let create_persisted = b_result??;
        a_result??;

        // Final state must be one of the valid serialized results.
        let final_store = testkit::open_file(&database_path).await?;
        let final_repository: Arc<dyn PlacementRepository> = Arc::new(final_store.clone());
        let final_placement = PlacementLedger::open(&placement_root, final_repository).await?;
        let provider = final_placement.provider("node-a").await?;
        let resource_present = final_store.get_resource(server_id).await.is_ok();
        let allocation_present = provider.allocations.contains_key(&allocation_id);
        if create_persisted {
            assert!(
                allocation_present,
                "a persisted consumer must never lose its allocation"
            );
        } else {
            assert!(
                !resource_present,
                "an aborted create must not persist consumer rows"
            );
        }
        let used: u64 = provider
            .allocations
            .values()
            .map(|allocation| allocation.resources.get(VCPU).copied().unwrap_or_default())
            .sum();
        assert_eq!(
            provider.inventories[VCPU].used, used,
            "usage must always equal the sum of surviving allocations"
        );

        let _ = std::fs::remove_file(&database_path);
        let _ = std::fs::remove_dir_all(&placement_root);
    }

    let _ = std::fs::remove_file(&harness.database_path);
    let _ = std::fs::remove_dir_all(&harness.placement_root);
    Ok(())
}
