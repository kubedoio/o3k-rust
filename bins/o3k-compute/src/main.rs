use std::{env, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use o3k_compute_agent::{
    AgentClient, AgentConfig, AgentError, CommandExecutionResult, CommandExecutor,
    ConsoleLogResult, TlsFiles,
};
use o3k_libvirt::{LibvirtAdapter, LibvirtConfig, stable_domain_name};
use o3k_provider_contract::compute_proto as proto;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Clone)]
struct HealthState {
    agent: AgentClient,
    libvirt_ready: bool,
    libvirt_error: Option<String>,
}

struct LibvirtCommandExecutor {
    adapter: LibvirtAdapter,
}

#[async_trait]
impl CommandExecutor for LibvirtCommandExecutor {
    async fn execute(
        &self,
        command: &proto::Command,
    ) -> Result<CommandExecutionResult, AgentError> {
        let name = stable_domain_name(&command.resource_id);
        let success = |message: &str| {
            Ok(CommandExecutionResult {
                state: proto::OperationState::Succeeded as i32,
                error_category: proto::ErrorCategory::Unspecified as i32,
                redacted_message: message.to_owned(),
                provider_resource_id: name.clone(),
                console_log: None,
            })
        };
        match command.action.as_ref() {
            Some(proto::command::Action::Inspect(_)) => {
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                success(if inspection.active {
                    "domain is active"
                } else {
                    "domain is inactive"
                })
            }
            Some(proto::command::Action::Start(_)) => {
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                self.adapter
                    .start(name.clone())
                    .await
                    .map_err(agent_error)?;
                success("domain started")
            }
            Some(proto::command::Action::Stop(_)) => {
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                self.adapter
                    .shutdown(name.clone())
                    .await
                    .map_err(agent_error)?;
                success("domain stopped")
            }
            Some(proto::command::Action::Reboot(_)) => {
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                self.adapter
                    .reboot(name.clone())
                    .await
                    .map_err(agent_error)?;
                success("domain rebooted")
            }
            Some(proto::command::Action::Delete(_)) => {
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                if inspection.active {
                    self.adapter
                        .force_stop(name.clone())
                        .await
                        .map_err(agent_error)?;
                }
                self.adapter
                    .undefine(name.clone())
                    .await
                    .map_err(agent_error)?;
                success("domain deleted")
            }
            Some(proto::command::Action::Create(_)) => Err(AgentError::Protocol(
                "create command requires a resolved domain definition".to_owned(),
            )),
            Some(proto::command::Action::ConsoleLog(request)) => {
                if request.offset > 0 {
                    return Err(AgentError::Protocol(
                        "libvirt console snapshots only support offset zero".to_owned(),
                    ));
                }
                let max_bytes = usize::try_from(request.max_bytes)
                    .map_err(|_| AgentError::Protocol("console bound is invalid".to_owned()))?
                    .min(o3k_console::MAX_CONSOLE_BYTES);
                if max_bytes == 0 {
                    return Err(AgentError::Protocol("console bound is invalid".to_owned()));
                }
                let bytes = self
                    .adapter
                    .read_console(name.clone(), max_bytes)
                    .await
                    .map_err(agent_error)?;
                Ok(CommandExecutionResult {
                    state: proto::OperationState::Succeeded as i32,
                    error_category: proto::ErrorCategory::Unspecified as i32,
                    redacted_message: "libvirt console output read".to_owned(),
                    provider_resource_id: name,
                    console_log: Some(ConsoleLogResult {
                        truncated: bytes.len() == max_bytes,
                        complete: bytes.len() < max_bytes,
                        offset: 0,
                        bytes,
                    }),
                })
            }
            None => Err(AgentError::Protocol("command action is missing".to_owned())),
        }
    }
}

fn verify_owned_domain(
    inspection: &o3k_libvirt::DomainInspection,
    expected_server_id: &str,
) -> Result<(), AgentError> {
    match o3k_libvirt::discover_domain_xml(&inspection.name, &inspection.xml) {
        o3k_libvirt::DiscoveryResult::Owned { metadata, .. }
            if metadata.server_id == expected_server_id =>
        {
            Ok(())
        }
        _ => Err(AgentError::Protocol(
            "libvirt domain ownership verification failed".to_owned(),
        )),
    }
}

fn agent_error(_error: o3k_libvirt::LibvirtError) -> AgentError {
    AgentError::Protocol("libvirt command failed".to_owned())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = config_from_env()?;
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let libvirt = LibvirtAdapter::new(LibvirtConfig::default())?;
    let (libvirt_ready, libvirt_error) = match libvirt.capabilities().await {
        Ok(capabilities) => {
            config.capabilities = capabilities.to_protocol_capabilities();
            (true, None)
        }
        Err(error) => {
            let message = error.to_string();
            tracing::warn!(error = %message, "local libvirt is unavailable");
            (false, Some(message))
        }
    };
    let agent = AgentClient::new(config.clone())?;
    let executor = Arc::new(LibvirtCommandExecutor {
        adapter: libvirt.clone(),
    });
    info!(endpoint = %config.endpoint, host_label = %config.host_label, "o3k-compute starting");
    let health_addr = env::var("O3K_COMPUTE_HEALTH_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:9100".to_owned())
        .parse::<SocketAddr>()?;
    let state = HealthState {
        agent: agent.clone(),
        libvirt_ready,
        libvirt_error,
    };
    let health_server = axum::serve(TcpListener::bind(health_addr).await?, health_router(state));
    tokio::select! {
        result = agent.run_with_executor(shutdown_signal(), executor) => { result?; }
        result = health_server.with_graceful_shutdown(shutdown_signal()) => { result?; }
    }
    info!("o3k-compute stopped");
    Ok(())
}

fn health_router(state: HealthState) -> Router {
    Router::new()
        .route("/healthz", get(liveness))
        .route("/readyz", get(readiness))
        .route("/metrics", get(metrics))
        .with_state(state)
}

async fn liveness() -> impl IntoResponse {
    (StatusCode::OK, "{\"status\":\"alive\"}\n")
}

async fn readiness(State(state): State<HealthState>) -> impl IntoResponse {
    if state.agent.is_ready() && state.libvirt_ready {
        (StatusCode::OK, "{\"status\":\"ready\"}\n".to_owned())
    } else {
        let error = state
            .libvirt_error
            .as_deref()
            .unwrap_or("control plane is not connected");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "{{\"status\":\"not_ready\",\"reason\":{}}}\n",
                serde_json::to_string(error).unwrap_or_else(|_| "\"unavailable\"".to_owned())
            ),
        )
    }
}

async fn metrics(State(state): State<HealthState>) -> impl IntoResponse {
    let ready = u8::from(state.agent.is_ready() && state.libvirt_ready);
    (StatusCode::OK, format!("o3k_compute_ready {ready}\n"))
}

async fn shutdown_signal() {
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

fn config_from_env() -> Result<AgentConfig, Box<dyn std::error::Error>> {
    let data_dir = PathBuf::from(
        env::var("O3K_COMPUTE_DATA_DIR").unwrap_or_else(|_| "./compute-data".to_owned()),
    );
    let endpoint = env::var("O3K_COMPUTE_CONTROL_ENDPOINT")
        .unwrap_or_else(|_| "https://127.0.0.1:50051".to_owned());
    let server_name =
        env::var("O3K_COMPUTE_SERVER_NAME").unwrap_or_else(|_| "o3k-control-plane".to_owned());
    let host_label =
        env::var("O3K_COMPUTE_HOST_LABEL").unwrap_or_else(|_| "compute-host".to_owned());
    let software_version =
        env::var("O3K_COMPUTE_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_owned());
    let tls_dir = PathBuf::from(
        env::var("O3K_COMPUTE_TLS_DIR").unwrap_or_else(|_| "./compute-tls".to_owned()),
    );
    Ok(AgentConfig {
        endpoint,
        server_name,
        tls: TlsFiles {
            ca_certificate: tls_dir.join("ca.pem"),
            certificate: tls_dir.join("agent.pem"),
            private_key: tls_dir.join("agent-key.pem"),
        },
        identity_file: data_dir.join("agent-id"),
        host_label,
        software_version,
        heartbeat_interval: Duration::from_secs(5),
        max_reconnect_delay: Duration::from_secs(30),
        capabilities: o3k_provider_contract::compute_proto::Capabilities {
            architecture: env::consts::ARCH.to_owned(),
            agent_provider_name: "o3k-compute".to_owned(),
            agent_provider_version: env!("CARGO_PKG_VERSION").to_owned(),
            ..Default::default()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspection(xml: &str) -> o3k_libvirt::DomainInspection {
        o3k_libvirt::DomainInspection {
            name: "o3k-domain".to_owned(),
            active: false,
            persistent: true,
            state: "shutoff".to_owned(),
            max_memory_kib: 128 * 1024,
            vcpus: 1,
            xml: xml.to_owned(),
        }
    }

    #[test]
    fn lifecycle_mutations_require_matching_owned_metadata() {
        let xml = "<domain><metadata><o3k:domain xmlns:o3k=\"urn:o3k:compute:domain\" server_id=\"server-1\" project_id=\"project\" generation=\"1\" operation_id=\"operation\" managed_by=\"o3k-compute\" /></metadata></domain>";
        assert!(verify_owned_domain(&inspection(xml), "server-1").is_ok());
        assert!(verify_owned_domain(&inspection(xml), "server-2").is_err());
        assert!(verify_owned_domain(&inspection("<domain />"), "server-1").is_err());
    }
}
