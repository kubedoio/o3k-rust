//! Durable, bounded bridge from the synchronous kernel audit port to the store.
//!
//! The kernel port predates asynchronous persistence and cannot return a write
//! error. This adapter therefore makes backpressure explicit and exposes
//! health so composition/readiness can fail closed; it never silently drops a
//! queued event.

use std::sync::{
    Arc, Mutex,
    mpsc::{Receiver, SyncSender, sync_channel},
};
use std::thread::JoinHandle;
use std::time::Duration;

use o3k_kernel::{AuditEvent, AuditSink, AuditSinkError};

use crate::{AuditEventRecord, O3kStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableAuditHealth {
    Running,
    Failed,
    Closed,
}

struct Message {
    event: AuditEvent,
    acknowledged: std::sync::mpsc::SyncSender<Result<(), ()>>,
}

// Audit admission is on a synchronous mutation path.  Never allow a stalled
// database writer (or a permanently full queue) to hold an HTTP worker forever.
// A timeout fails closed; the caller must not perform the mutation after this
// sink reports unavailable.
const ACK_TIMEOUT: Duration = Duration::from_secs(30);

pub struct DurableAuditSink {
    sender: Option<SyncSender<Message>>,
    health: Arc<Mutex<DurableAuditHealth>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl DurableAuditSink {
    pub fn start(store: Arc<O3kStore>, capacity: usize) -> Result<Arc<Self>, String> {
        if capacity == 0 {
            return Err("audit queue capacity must be positive".into());
        }
        let (sender, receiver) = sync_channel(capacity);
        let health = Arc::new(Mutex::new(DurableAuditHealth::Running));
        let worker_health = Arc::clone(&health);
        let worker = std::thread::Builder::new()
            .name("o3k-audit-writer".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        *worker_health.lock().unwrap_or_else(|e| e.into_inner()) =
                            DurableAuditHealth::Failed;
                        return;
                    }
                };
                run_writer(runtime, store, receiver, worker_health);
            })
            .map_err(|error| format!("audit writer thread failed: {error}"))?;
        Ok(Arc::new(Self {
            sender: Some(sender),
            health,
            worker: Mutex::new(Some(worker)),
        }))
    }

    #[must_use]
    pub fn health(&self) -> DurableAuditHealth {
        *self.health.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn run_writer(
    runtime: tokio::runtime::Runtime,
    store: Arc<O3kStore>,
    receiver: Receiver<Message>,
    health: Arc<Mutex<DurableAuditHealth>>,
) {
    while let Ok(message) = receiver.recv() {
        let record = match AuditEventRecord::from_kernel_event(&message.event) {
            Ok(record) => record,
            Err(_) => {
                let _ = message.acknowledged.send(Err(()));
                *health.lock().unwrap_or_else(|e| e.into_inner()) = DurableAuditHealth::Failed;
                continue;
            }
        };
        let mut persisted = false;
        for attempt in 0..3u32 {
            // Bound the database operation itself as well as the caller's
            // acknowledgement wait.  Without this timeout a wedged socket
            // or database lock could leave the writer thread stuck forever,
            // making Drop/restart unable to establish a clean durability
            // boundary and preventing health from becoming sticky Failed.
            let write_result = runtime.block_on(async {
                tokio::time::timeout(
                    ACK_TIMEOUT,
                    <O3kStore as crate::AuditRepository>::append_audit_event(&store, &record),
                )
                .await
            });
            if matches!(write_result, Ok(Ok(()))) {
                persisted = true;
                break;
            }
            if attempt < 2 {
                runtime.block_on(tokio::time::sleep(std::time::Duration::from_millis(
                    25 * (1u64 << attempt),
                )));
            }
        }
        if !persisted {
            let _ = message.acknowledged.send(Err(()));
            *health.lock().unwrap_or_else(|e| e.into_inner()) = DurableAuditHealth::Failed;
            // Stop at the first durable write failure. Continuing would consume
            // later events while leaving a permanent hole in the audit stream.
            // The failed health state is intentionally sticky until the owning
            // process replaces/restarts the sink.
            return;
        }
        let _ = message.acknowledged.send(Ok(()));
    }
    let mut state = health.lock().unwrap_or_else(|e| e.into_inner());
    if *state == DurableAuditHealth::Running {
        *state = DurableAuditHealth::Closed;
    }
}

impl AuditSink for DurableAuditSink {
    fn ensure_available(&self) -> Result<(), AuditSinkError> {
        if self.health() == DurableAuditHealth::Running && self.sender.is_some() {
            Ok(())
        } else {
            Err(AuditSinkError::Unavailable)
        }
    }

    fn record(&self, event: &AuditEvent) {
        let _ = self.record_checked(event);
    }

    fn record_checked(&self, event: &AuditEvent) -> Result<(), AuditSinkError> {
        self.ensure_available()?;
        let Some(sender) = self.sender.as_ref() else {
            *self.health.lock().unwrap_or_else(|e| e.into_inner()) = DurableAuditHealth::Failed;
            return Err(AuditSinkError::Unavailable);
        };
        let (acknowledged, result) = std::sync::mpsc::sync_channel(0);
        let send_failed = sender
            .try_send(Message {
                event: event.clone(),
                acknowledged,
            })
            .is_err();
        // `try_send` deliberately rejects a full queue instead of applying
        // unbounded backpressure to request handlers.  The worker's bounded
        // queue remains durable, while overload fails closed at admission.
        let persistence_failed =
            !send_failed && !matches!(result.recv_timeout(ACK_TIMEOUT), Ok(Ok(())));
        if send_failed || persistence_failed {
            *self.health.lock().unwrap_or_else(|e| e.into_inner()) = DurableAuditHealth::Failed;
            return Err(AuditSinkError::Unavailable);
        }
        Ok(())
    }
}

impl Drop for DurableAuditSink {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.get_mut().ok().and_then(Option::take) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use o3k_kernel::{
        ActionId, AuditOutcome, AuthContext, OwnershipScope, Principal, PrincipalId, ResourceType,
        ScopeId, ServiceNamespace, UserPrincipal,
    };
    use uuid::Uuid;

    fn test_event() -> AuditEvent {
        let auth = AuthContext::new(
            Principal::User(UserPrincipal::new(
                PrincipalId::new_unchecked("user-a"),
                "user-a",
                None,
            )),
            OwnershipScope::project(ScopeId::new_unchecked("project-a"), None, None),
            vec!["member".into()],
            1,
            2,
            "audit-a",
            "request-a",
            None,
        );
        AuditEvent::from_auth(
            &auth,
            ServiceNamespace::new("compute").expect("valid namespace"),
            ActionId::new("compute", "ReadServer").expect("valid action"),
            AuditOutcome::Succeeded,
        )
        .with_resource(
            ResourceType::new("compute", "server").expect("valid resource type"),
            None,
            None,
        )
    }

    #[tokio::test]
    async fn durable_sink_flushes_before_drop_and_survives_reopen() {
        let path = std::env::temp_dir().join(format!("o3k-audit-sink-{}.sqlite", Uuid::new_v4()));
        let store = Arc::new(
            O3kStore::connect_sqlite_file(&path)
                .await
                .expect("open sqlite store"),
        );
        let sink = DurableAuditSink::start(store.clone(), 1).expect("start sink");
        let event = test_event();
        let event_id = event.event_id.to_string();
        let scope = event.effective_scope.to_string();
        sink.record(&event);
        drop(sink);

        let persisted =
            <O3kStore as crate::AuditRepository>::get_audit_event(&store, &scope, &event_id)
                .await
                .expect("read persisted event");
        assert!(
            persisted.is_some(),
            "sink drop must flush queued audit data"
        );
        drop(store);

        let reopened = O3kStore::connect_sqlite_file(&path)
            .await
            .expect("reopen sqlite store");
        assert!(
            <O3kStore as crate::AuditRepository>::get_audit_event(&reopened, &scope, &event_id,)
                .await
                .expect("read after reopen")
                .is_some()
        );
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn failed_sink_rejects_mutation_admission_and_recording() {
        let store = Arc::new(
            O3kStore::connect_sqlite_memory()
                .await
                .expect("open sqlite"),
        );
        let sink = DurableAuditSink::start(store, 1).expect("start sink");
        *sink.health.lock().unwrap_or_else(|e| e.into_inner()) = DurableAuditHealth::Failed;

        assert_eq!(sink.ensure_available(), Err(AuditSinkError::Unavailable));
        assert_eq!(
            sink.record_checked(&test_event()),
            Err(AuditSinkError::Unavailable)
        );
    }

    #[test]
    fn direct_untrusted_reason_is_normalized_before_durable_serialization() {
        let mut event = test_event();
        event.reason_category = Some("provider password=super-secret".to_owned());

        let record = AuditEventRecord::from_kernel_event(&event).expect("project event");
        assert_eq!(record.reason_category.as_deref(), Some("operation_failed"));
        assert!(!record.event_json.contains("super-secret"));
        assert!(!record.event_json.contains("provider password"));
    }
}
