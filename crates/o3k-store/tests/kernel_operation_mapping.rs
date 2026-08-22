use o3k_kernel::OperationState as KernelOperationState;
use o3k_store::{
    DurableStore, OperationRecord, OperationState, ResourceRecord, SqliteStore, StoreError,
};
use uuid::Uuid;

#[test]
fn durable_and_kernel_operation_states_map_one_to_one() {
    let states = [
        OperationState::Pending,
        OperationState::Running,
        OperationState::Succeeded,
        OperationState::Retryable,
        OperationState::UnknownOutcome,
        OperationState::Failed,
    ];

    for durable in states {
        let kernel: KernelOperationState = durable.into();
        let round_trip: OperationState = kernel.into();
        assert_eq!(round_trip, durable);
    }
}

/// Pre-P12 journal rows intentionally have no canonical public identity.
/// They remain available to internal recovery through `get_operation`, while
/// native Kernel reconstruction fails closed instead of inventing metadata.
#[tokio::test]
async fn legacy_operation_without_canonical_metadata_remains_internal() -> Result<(), StoreError> {
    let store = SqliteStore::connect("sqlite::memory:").await?;
    let resource_id = Uuid::now_v7();
    let operation_id = Uuid::now_v7();
    store
        .insert_resource(&ResourceRecord {
            id: resource_id,
            kind: "compute_instance".into(),
            project_id: "legacy-project".into(),
            generation: 1,
            observed_generation: 0,
            desired_state: "requested".into(),
            observed_state: "unknown".into(),
            provider_id: None,
        })
        .await?;
    let legacy = OperationRecord {
        id: operation_id,
        resource_id,
        kind: "create".into(),
        state: OperationState::UnknownOutcome,
        provider_operation_id: Some("legacy-provider-operation".into()),
        error_category: Some("unknown_outcome".into()),
        error_message: None,
    };
    store.insert_operation(&legacy).await?;

    assert_eq!(store.get_operation(operation_id).await?, legacy);
    assert!(matches!(
        store.get_canonical_operation(operation_id).await,
        Err(StoreError::OperationNotFound)
    ));
    Ok(())
}
