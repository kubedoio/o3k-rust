use o3k_console;
use std::sync::Arc;
use tracing;

pub(crate) fn spawn_console_event_consumer(
    mut events: tokio::sync::broadcast::Receiver<o3k_provider::AgentEvent>,
    console: o3k_console::ConsoleService,
    store: Arc<dyn o3k_store::DurableStore>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(o3k_provider::AgentEvent::Observation(observation))
                    if !observation.console_log_bytes.is_empty() =>
                {
                    // Liveness guard (issue #89, defect 4): after a crash the
                    // agent re-delivers committed journal observations. For a
                    // server whose delete already completed, the delete path's
                    // `console.cleanup` removed the console log, so writing
                    // the replayed bytes would resurrect owned host state that
                    // must stay absent. The delete projection keeps a DELETED
                    // tombstone (the row is never removed), so a durable read
                    // decides: only a present, decodable, non-Deleted resource
                    // may receive console bytes. Anything else is stale replay
                    // evidence and is skipped.
                    let resource_is_live = match store.get_resource(observation.resource_id).await {
                        Ok(resource) => {
                            match o3k_store::server_state_from_storage(&resource.observed_state) {
                                Ok(o3k_domain::ServerState::Deleted) => {
                                    tracing::debug!(
                                        resource_id = %observation.resource_id,
                                        "skipping console observation for deleted resource"
                                    );
                                    false
                                }
                                Ok(_) => true,
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        resource_id = %observation.resource_id,
                                        "skipping console observation for resource with corrupt state"
                                    );
                                    false
                                }
                            }
                        }
                        Err(o3k_store::StoreError::ResourceNotFound) => {
                            tracing::debug!(
                                resource_id = %observation.resource_id,
                                "skipping console observation for absent resource"
                            );
                            false
                        }
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                resource_id = %observation.resource_id,
                                "skipping console observation: resource liveness could not be verified"
                            );
                            false
                        }
                    };
                    if !resource_is_live {
                        continue;
                    }
                    if let Err(error) = console.write_chunk(
                        observation.resource_id,
                        observation.console_log_offset,
                        &observation.console_log_bytes,
                    ) {
                        tracing::warn!(%error, resource_id = %observation.resource_id, "agent console observation was rejected");
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                    tracing::warn!(count, "console observation consumer lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

pub(crate) async fn control_shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            let _ = signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}
