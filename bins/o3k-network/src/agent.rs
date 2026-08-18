//! Transport boundary for the node-local network executor.

use o3k_network::{
    NetworkAgentIdentity, NetworkControllerLease, NetworkExecutionError, NetworkPlanCommand,
    NetworkPlanExecutor, NetworkPlanRealizer, PlanAdmission,
};
use std::{
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tonic::{Request, Response, Status};
use uuid::Uuid;

pub mod proto {
    tonic::include_proto!("o3k.network.v1");
}

use proto::{
    CommandResult, ControlRequest, ControlResponse, ProtocolError, RegisterAck,
    control_request::Body as RequestBody, control_response::Body as ResponseBody,
    network_agent_server::NetworkAgent,
};

const PROTOCOL_MAJOR: u32 = 1;
const PROTOCOL_MINOR: u32 = 0;

#[derive(Debug, thiserror::Error)]
enum NetworkAgentError {
    #[error("network agent runtime is poisoned")]
    Poisoned,
    #[error("network agent command is malformed: {0}")]
    Malformed(&'static str),
    #[error("network plan payload is invalid")]
    InvalidPlan,
}

struct Runtime<R> {
    executor: NetworkPlanExecutor,
    realizer: R,
    registered: bool,
}

pub struct NetworkAgentService<R> {
    runtime: Arc<Mutex<Runtime<R>>>,
}

impl<R> Clone for NetworkAgentService<R> {
    fn clone(&self) -> Self {
        Self {
            runtime: Arc::clone(&self.runtime),
        }
    }
}

impl<R> NetworkAgentService<R>
where
    R: NetworkPlanRealizer + Send + 'static,
{
    pub fn new(executor: NetworkPlanExecutor, realizer: R) -> Self {
        Self {
            runtime: Arc::new(Mutex::new(Runtime {
                executor,
                realizer,
                registered: false,
            })),
        }
    }

    fn register(&self, request: &proto::Register) -> Result<RegisterAck, NetworkAgentError> {
        let runtime = self
            .runtime
            .lock()
            .map_err(|_| NetworkAgentError::Poisoned)?;
        if request.agent_id != runtime.executor.agent_id()
            || request.agent_epoch != runtime.executor.agent_epoch()
        {
            return Err(NetworkAgentError::Malformed("stale agent identity"));
        }
        drop(runtime);
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| NetworkAgentError::Poisoned)?;
        runtime.registered = true;
        Ok(RegisterAck {
            agent_id: request.agent_id.clone(),
            agent_epoch: request.agent_epoch.clone(),
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
        })
    }

    fn execute(&self, command: &proto::NetworkCommand) -> Result<CommandResult, NetworkAgentError> {
        let command_id = parse_uuid(&command.command_id, "command_id")?;
        let operation_id = parse_uuid(&command.operation_id, "operation_id")?;
        let plan =
            serde_json::from_str(&command.plan_json).map_err(|_| NetworkAgentError::InvalidPlan)?;
        let internal = NetworkPlanCommand {
            command_id,
            operation_id,
            idempotency_key: command.idempotency_key.clone(),
            target: NetworkAgentIdentity {
                agent_id: command.agent_id.clone(),
                agent_epoch: command.agent_epoch.clone(),
            },
            controller: NetworkControllerLease {
                controller_id: command.controller_id.clone(),
                controller_epoch: command.controller_epoch.clone(),
                fencing_token: command.fencing_token,
            },
            deadline_unix_ms: command.deadline_unix_ms,
            plan,
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| NetworkAgentError::Malformed("system clock before epoch"))?
            .as_millis() as u64;
        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_| NetworkAgentError::Poisoned)?;
        let Runtime {
            executor,
            realizer,
            registered,
        } = &mut *runtime;
        if !*registered {
            return Err(NetworkAgentError::Malformed("register is required first"));
        }
        let admission = match executor.execute(&internal, now, realizer) {
            Ok(admission) => admission,
            Err(NetworkExecutionError::MutationOutcomeUnknown) => {
                return Ok(CommandResult {
                    command_id: command.command_id.clone(),
                    status: "unknown".to_owned(),
                    replayed: false,
                    error_code: "mutation_outcome_unknown".to_owned(),
                });
            }
            Err(_) => return Err(NetworkAgentError::Malformed("command rejected")),
        };
        let (status, replayed) = match admission {
            PlanAdmission::Accepted => ("succeeded", false),
            PlanAdmission::Replayed => ("replayed", true),
            PlanAdmission::RequiresObservation => ("requires_observation", true),
        };
        Ok(CommandResult {
            command_id: command.command_id.clone(),
            status: status.to_owned(),
            replayed,
            error_code: String::new(),
        })
    }
}

#[tonic::async_trait]
impl<R> NetworkAgent for NetworkAgentService<R>
where
    R: NetworkPlanRealizer + Send + 'static,
{
    type ControlStream = ReceiverStream<Result<ControlResponse, Status>>;

    async fn control(
        &self,
        request: Request<tonic::Streaming<ControlRequest>>,
    ) -> Result<Response<Self::ControlStream>, Status> {
        let mut input = request.into_inner();
        let service = (*self).clone();
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            let mut registered = false;
            while let Some(message) = input.next().await {
                let response = match message {
                    Ok(ControlRequest {
                        body: Some(RequestBody::Register(register)),
                    }) => match service.register(&register) {
                        Ok(ack) => {
                            registered = true;
                            ControlResponse {
                                body: Some(ResponseBody::Register(ack)),
                            }
                        }
                        Err(_) => error_response("stale_or_invalid_registration"),
                    },
                    Ok(ControlRequest {
                        body: Some(RequestBody::Command(command)),
                    }) if registered => match service.execute(&command) {
                        Ok(result) => ControlResponse {
                            body: Some(ResponseBody::Result(result)),
                        },
                        Err(error) => error_response(error_code(&error)),
                    },
                    Ok(ControlRequest {
                        body: Some(RequestBody::Command(_)),
                    }) => error_response("register_required"),
                    Ok(ControlRequest { body: None }) => error_response("empty_request"),
                    Err(_) => error_response("malformed_request"),
                };
                if tx.send(Ok(response)).await.is_err() {
                    break;
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

fn parse_uuid(value: &str, field: &'static str) -> Result<Uuid, NetworkAgentError> {
    Uuid::parse_str(value).map_err(|_| NetworkAgentError::Malformed(field))
}

fn error_response(code: &str) -> ControlResponse {
    ControlResponse {
        body: Some(ResponseBody::Error(ProtocolError {
            code: code.to_owned(),
        })),
    }
}

fn error_code(error: &NetworkAgentError) -> &'static str {
    match error {
        NetworkAgentError::Poisoned => "runtime_poisoned",
        NetworkAgentError::Malformed(code) => code,
        NetworkAgentError::InvalidPlan => "invalid_plan",
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use prost::Message;

    struct NoopRealizer;

    impl NetworkPlanRealizer for NoopRealizer {
        type Error = std::convert::Infallible;

        fn realize(&mut self, _plan: &o3k_network::NodeNetworkPlan) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn network_command_wire_round_trip_preserves_fencing_identity() {
        let request = ControlRequest {
            body: Some(RequestBody::Command(proto::NetworkCommand {
                command_id: "command".to_owned(),
                operation_id: "operation".to_owned(),
                controller_id: "controller".to_owned(),
                controller_epoch: "epoch".to_owned(),
                fencing_token: 7,
                ..Default::default()
            })),
        };
        let decoded = ControlRequest::decode(request.encode_to_vec().as_slice())
            .expect("network command must decode");
        assert!(matches!(&decoded.body, Some(RequestBody::Command(_))));
        let Some(RequestBody::Command(command)) = decoded.body else {
            return;
        };
        assert_eq!(command.controller_id, "controller");
        assert_eq!(command.controller_epoch, "epoch");
        assert_eq!(command.fencing_token, 7);
    }

    #[test]
    fn registration_rejects_a_stale_agent_epoch() {
        let root = std::env::temp_dir().join(format!("o3k-network-agent-{}", Uuid::now_v7()));
        let executor = NetworkPlanExecutor::open(
            &root,
            NetworkAgentIdentity {
                agent_id: "agent-a".to_owned(),
                agent_epoch: "epoch-2".to_owned(),
            },
            NetworkControllerLease {
                controller_id: "controller".to_owned(),
                controller_epoch: "epoch-1".to_owned(),
                fencing_token: 1,
            },
        )
        .expect("executor");
        let service = NetworkAgentService::new(executor, NoopRealizer);
        let result = service.register(&proto::Register {
            agent_id: "agent-a".to_owned(),
            agent_epoch: "epoch-1".to_owned(),
        });
        assert!(matches!(
            result,
            Err(NetworkAgentError::Malformed("stale agent identity"))
        ));
        let _ = std::fs::remove_dir_all(root);
    }
}
