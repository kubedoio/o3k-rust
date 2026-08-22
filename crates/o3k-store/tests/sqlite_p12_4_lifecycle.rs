use o3k_store::{
    CanonicalOperationLifecycleUpdate, CanonicalOperationRecord, DurableStore,
    IdempotencyReservationRequest, OperationRecord, OperationState, ResourceRecord, SqliteStore,
    StoreError,
};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn sqlite_p12_4_canonical_lifecycle_updates_and_reopens() -> Result<(), StoreError> {
    let path = std::env::temp_dir().join(format!("o3k-p12-4-lifecycle-{}", Uuid::now_v7()));
    let url = format!("sqlite://{}", path.display());
    let operation_id = Uuid::now_v7();
    let resource_id = Uuid::now_v7();
    let operation = OperationRecord {
        id: operation_id,
        resource_id,
        kind: "lifecycle:create".into(),
        state: OperationState::Pending,
        provider_operation_id: None,
        error_category: None,
        error_message: None,
    };
    let canonical = CanonicalOperationRecord {
        id: operation_id,
        service: "compute".into(),
        action: "compute:CreateServer".into(),
        actor: "user".into(),
        owner_scope: "project-a".into(),
        resource_type: "compute:server".into(),
        resource_id: Some(resource_id.to_string()),
        state: OperationState::Pending,
        attempt: 0,
        created_at: "2026-01-01T00:00:00Z".into(),
        started_at: None,
        finished_at: None,
        error: None,
        request_id: Some("request".into()),
    };
    let request = IdempotencyReservationRequest::from_semantics(
        "project-a",
        "compute:CreateServer",
        "lifecycle-key",
        "compute:server",
        None,
        &json!({"name":"demo"}),
        operation_id,
    )?;
    {
        let store = SqliteStore::connect(&url).await?;
        store
            .insert_resource(&ResourceRecord {
                id: resource_id,
                kind: "compute:server".into(),
                project_id: "project-a".into(),
                generation: 1,
                observed_generation: 0,
                desired_state: "pending".into(),
                observed_state: "unknown".into(),
                provider_id: None,
            })
            .await?;
        store
            .create_or_replay_canonical_idempotent_operation(&operation, &canonical, &request)
            .await?;

        let started = "2026-01-01T00:01:00Z".to_owned();
        let running = store
            .update_canonical_operation_lifecycle(
                operation_id,
                &CanonicalOperationLifecycleUpdate::new(
                    o3k_kernel::OperationState::Running,
                    1,
                    Some(started.clone()),
                    None,
                    None,
                )?,
            )
            .await?;
        assert_eq!(running.state, OperationState::Running);
        assert_eq!(running.attempt, 1);
        assert_eq!(running.started_at.as_deref(), Some(started.as_str()));
        assert_eq!(running.finished_at, None);

        let finished = "2026-01-01T00:02:00Z".to_owned();
        let succeeded = store
            .update_canonical_operation_lifecycle(
                operation_id,
                &CanonicalOperationLifecycleUpdate::new(
                    o3k_kernel::OperationState::Succeeded,
                    1,
                    Some(started.clone()),
                    Some(finished.clone()),
                    None,
                )?,
            )
            .await?;
        assert_eq!(succeeded.state, OperationState::Succeeded);
        assert_eq!(succeeded.attempt, 1);
        assert_eq!(succeeded.started_at.as_deref(), Some(started.as_str()));
        assert_eq!(succeeded.finished_at.as_deref(), Some(finished.as_str()));
        assert_eq!(succeeded.error, None);

        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Succeeded
        );

        let invalid = CanonicalOperationLifecycleUpdate {
            state: OperationState::Running,
            attempt: 2,
            started_at: None,
            finished_at: None,
            public_error: None,
        };
        assert!(
            store
                .update_canonical_operation_lifecycle(operation_id, &invalid)
                .await
                .is_err()
        );
        assert_eq!(
            store.get_operation(operation_id).await?.state,
            OperationState::Succeeded
        );
    }

    let reopened = SqliteStore::connect(&url).await?;
    let restored = reopened.get_canonical_operation(operation_id).await?;
    assert_eq!(restored.state, OperationState::Succeeded);
    assert_eq!(restored.attempt, 1);
    assert_eq!(
        restored.finished_at.as_deref(),
        Some("2026-01-01T00:02:00Z")
    );
    let kernel = o3k_kernel::Operation::try_from(restored)?;
    assert_eq!(kernel.state, o3k_kernel::OperationState::Succeeded);
    assert_eq!(kernel.attempt, 1);
    assert_eq!(kernel.finished_at.as_deref(), Some("2026-01-01T00:02:00Z"));
    Ok(())
}
