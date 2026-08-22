#![allow(clippy::unwrap_used, clippy::expect_used)]

use o3k_store::{
    CanonicalOperationRecord, DurableStore, IdempotencyReservation, IdempotencyReservationRequest,
    OperationRecord, OperationState, PostgresStore, ResourceRecord,
};
use serde_json::json;
use uuid::Uuid;

fn url() -> String {
    std::env::var("O3K_DATABASE_URL")
        .expect("O3K_DATABASE_URL must be set for PostgreSQL P12.4 conformance")
}

fn resource(id: Uuid, project: &str) -> ResourceRecord {
    ResourceRecord {
        id,
        kind: "compute_server".into(),
        project_id: project.into(),
        generation: 1,
        observed_generation: 0,
        desired_state: "pending".into(),
        observed_state: "unknown".into(),
        provider_id: None,
    }
}

fn operation(id: Uuid, resource_id: Uuid) -> OperationRecord {
    OperationRecord {
        id,
        resource_id,
        kind: "lifecycle:create".into(),
        state: OperationState::Pending,
        provider_operation_id: None,
        error_category: None,
        error_message: None,
    }
}

#[tokio::test]
#[ignore = "requires the mandatory PostgreSQL P12.4 CI job"]
async fn postgres_p12_4_persistence_idempotency_and_cas() {
    let database_url = url();
    let store = PostgresStore::connect(&database_url)
        .await
        .expect("connect");
    store
        .clean_tables_for_testing()
        .await
        .expect("clean dedicated database");

    let resource_id = Uuid::now_v7();
    store
        .insert_resource(&resource(resource_id, "project-a"))
        .await
        .expect("resource");
    let operation_id = Uuid::now_v7();
    let op = operation(operation_id, resource_id);
    let canonical = CanonicalOperationRecord {
        id: operation_id,
        service: "compute".into(),
        action: "compute:CreateServer".into(),
        actor: "user-a".into(),
        owner_scope: "project-a".into(),
        resource_type: "compute:server".into(),
        resource_id: Some(resource_id.to_string()),
        state: OperationState::Pending,
        attempt: 0,
        created_at: "2026-01-01T00:00:00Z".into(),
        started_at: None,
        finished_at: None,
        error: None,
        request_id: Some("request-a".into()),
    };
    let request = IdempotencyReservationRequest::from_semantics(
        "project-a",
        "compute:CreateServer",
        "p12-4-replay",
        "compute:server",
        None,
        &json!({"name":"demo"}),
        operation_id,
    )
    .expect("request");
    assert_eq!(
        store
            .create_or_replay_idempotent_operation(&op, &request)
            .await
            .expect("create"),
        IdempotencyReservation::Created(operation_id)
    );
    store
        .insert_canonical_operation(&canonical)
        .await
        .expect("canonical metadata");
    drop(store);

    let reopened = PostgresStore::connect(&database_url)
        .await
        .expect("reconnect");
    assert_eq!(
        reopened
            .get_operation(operation_id)
            .await
            .expect("reload")
            .id,
        operation_id
    );
    let loaded = reopened
        .get_canonical_operation(operation_id)
        .await
        .expect("canonical reload");
    let kernel = o3k_kernel::Operation::try_from(loaded).expect("kernel conversion");
    assert_eq!(kernel.id, operation_id);
    assert_eq!(kernel.owner_scope.id().as_str(), "project-a");
    assert_eq!(kernel.action.as_str(), "compute:CreateServer");
    let replay = reopened
        .create_or_replay_idempotent_operation(&op, &request)
        .await
        .expect("replay");
    assert_eq!(
        replay,
        IdempotencyReservation::ExistingEquivalent(operation_id)
    );

    let stale = reopened
        .update_resource(resource_id, 0, "bad", "bad", 0, None)
        .await;
    assert!(stale.is_err(), "stale generation must fail before mutation");
}
