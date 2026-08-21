use o3k_kernel::OperationState as KernelOperationState;
use o3k_store::OperationState;

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
