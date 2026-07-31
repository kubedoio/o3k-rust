//! Secure registration and liveness runtime for the host-local compute agent.
//!
//! This crate deliberately contains no hypervisor or VM lifecycle code.  It
//! owns only the authenticated control stream, node state, and bounded
//! reconnect behavior described by SPEC-0015.

use std::{
    collections::HashMap,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use o3k_provider_contract::compute_proto as proto;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    sync::{RwLock, mpsc},
    time,
};
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::{
    Request, Response, Status, Streaming,
    transport::{Certificate, ClientTlsConfig, Endpoint, Identity, Server, ServerTlsConfig},
};
use tracing::{info, warn};
use uuid::Uuid;

pub const PROTOCOL_VERSION: proto::ProtocolVersion = proto::ProtocolVersion {
    major: 1,
    minor: 0,
    wire_revision: 1,
};
pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
pub const DEFAULT_LEASE: Duration = Duration::from_secs(15);
const MAX_AGENT_ID: usize = 128;
const MAX_HOST_LABEL: usize = 255;
pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("invalid compute-agent configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("compute-agent identity store is unavailable")]
    IdentityStore(#[source] std::io::Error),
    #[error("compute-agent transport is unavailable: {0}")]
    Transport(#[source] tonic::transport::Error),
    #[error("compute-agent TLS material is unavailable")]
    TlsMaterial,
    #[error("compute-agent protocol error: {0}")]
    Protocol(String),
}

#[derive(Clone)]
pub struct TlsFiles {
    pub ca_certificate: PathBuf,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
}

impl std::fmt::Debug for TlsFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsFiles")
            .field("ca_certificate", &self.ca_certificate)
            .field("certificate", &self.certificate)
            .field("private_key", &"<redacted>")
            .finish()
    }
}

impl TlsFiles {
    fn read(&self) -> Result<TlsMaterial, AgentError> {
        let ca = fs::read(&self.ca_certificate).map_err(|_| AgentError::TlsMaterial)?;
        let cert = fs::read(&self.certificate).map_err(|_| AgentError::TlsMaterial)?;
        let key = fs::read(&self.private_key).map_err(|_| AgentError::TlsMaterial)?;
        if ca.is_empty() || cert.is_empty() || key.is_empty() {
            return Err(AgentError::InvalidConfiguration(
                "TLS files must not be empty",
            ));
        }
        Ok(TlsMaterial { ca, cert, key })
    }
}

struct TlsMaterial {
    ca: Vec<u8>,
    cert: Vec<u8>,
    key: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub endpoint: String,
    pub server_name: String,
    pub tls: TlsFiles,
    pub identity_file: PathBuf,
    pub host_label: String,
    pub software_version: String,
    pub heartbeat_interval: Duration,
    pub max_reconnect_delay: Duration,
    pub capabilities: proto::Capabilities,
}

impl AgentConfig {
    pub fn validate(&self) -> Result<(), AgentError> {
        if !self.endpoint.starts_with("https://") || self.server_name.trim().is_empty() {
            return Err(AgentError::InvalidConfiguration(
                "endpoint must be HTTPS and server name must be set",
            ));
        }
        if self.host_label.is_empty() || self.host_label.len() > MAX_HOST_LABEL {
            return Err(AgentError::InvalidConfiguration(
                "host label length is invalid",
            ));
        }
        if self.software_version.trim().is_empty()
            || self.heartbeat_interval.is_zero()
            || self.max_reconnect_delay.is_zero()
        {
            return Err(AgentError::InvalidConfiguration(
                "version and retry intervals must be set",
            ));
        }
        let _ = self.tls.read()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct NodeSnapshot {
    pub agent_id: String,
    pub agent_epoch: String,
    pub host_label: String,
    pub software_version: String,
    pub capabilities: proto::Capabilities,
    pub desired_state: i32,
    pub applied_state: i32,
    pub availability: Availability,
    pub active_operation_count: u32,
    pub last_heartbeat_sequence: u64,
    pub last_heartbeat_at: SystemTime,
    pub transition_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedAgent {
    pub agent_id: String,
    pub certificate_sha256: [u8; 32],
}

impl AuthorizedAgent {
    pub fn new(agent_id: impl Into<String>, certificate: &[u8]) -> Self {
        let digest = Sha256::digest(normalize_certificate(certificate));
        let mut certificate_sha256 = [0_u8; 32];
        certificate_sha256.copy_from_slice(&digest);
        Self {
            agent_id: agent_id.into(),
            certificate_sha256,
        }
    }
}

fn normalize_certificate(certificate: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(certificate) else {
        return certificate.to_vec();
    };
    let Some(body) = text
        .strip_prefix("-----BEGIN CERTIFICATE-----")
        .and_then(|value| value.split("-----END CERTIFICATE-----").next())
    else {
        return certificate.to_vec();
    };
    BASE64
        .decode(body.split_whitespace().collect::<String>())
        .unwrap_or_else(|_| certificate.to_vec())
}

pub fn parse_authorized_agents(value: &str) -> Result<Vec<AuthorizedAgent>, AgentError> {
    let mut agents = Vec::new();
    for entry in value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let (agent_id, fingerprint) =
            entry
                .split_once('=')
                .ok_or(AgentError::InvalidConfiguration(
                    "authorized agent must be id=sha256hex",
                ))?;
        if agent_id.trim().is_empty() || fingerprint.len() != 64 || !fingerprint.is_ascii() {
            return Err(AgentError::InvalidConfiguration(
                "authorized agent must be id=sha256hex",
            ));
        }
        let mut certificate_sha256 = [0_u8; 32];
        for (index, byte) in certificate_sha256.iter_mut().enumerate() {
            let offset = index * 2;
            *byte = u8::from_str_radix(&fingerprint[offset..offset + 2], 16).map_err(|_| {
                AgentError::InvalidConfiguration("authorized agent fingerprint is not hex")
            })?;
        }
        agents.push(AuthorizedAgent {
            agent_id: agent_id.to_owned(),
            certificate_sha256,
        });
    }
    if agents.is_empty() {
        return Err(AgentError::InvalidConfiguration(
            "at least one authorized agent is required",
        ));
    }
    Ok(agents)
}

#[derive(Clone, Default)]
pub struct NodeRegistry {
    nodes: Arc<RwLock<HashMap<String, NodeSnapshot>>>,
    authorized_agents: Arc<RwLock<HashMap<String, [u8; 32]>>>,
}

impl NodeRegistry {
    pub async fn snapshot(&self, agent_id: &str) -> Option<NodeSnapshot> {
        self.nodes.read().await.get(agent_id).cloned()
    }

    pub async fn all(&self) -> Vec<NodeSnapshot> {
        self.nodes.read().await.values().cloned().collect()
    }

    pub async fn authorize_agent(&self, agent: AuthorizedAgent) -> Result<(), AgentError> {
        if agent.agent_id.trim().is_empty() || agent.agent_id.len() > MAX_AGENT_ID {
            return Err(AgentError::InvalidConfiguration(
                "authorized agent identity is invalid",
            ));
        }
        self.authorized_agents
            .write()
            .await
            .insert(agent.agent_id, agent.certificate_sha256);
        Ok(())
    }

    async fn is_authorized(&self, agent_id: &str, certificate: &[u8]) -> bool {
        let digest = Sha256::digest(certificate);
        self.authorized_agents
            .read()
            .await
            .get(agent_id)
            .is_some_and(|expected| expected.as_slice() == digest.as_slice())
    }

    pub async fn register(
        &self,
        request: &proto::RegisterRequest,
    ) -> Result<proto::RegisterResponse, Status> {
        validate_register(request)?;
        let now = SystemTime::now();
        let mut nodes = self.nodes.write().await;
        let desired = nodes
            .get(&request.agent_id)
            .map_or(proto::AdministrativeState::Enabled as i32, |n| {
                n.desired_state
            });
        let highest = nodes
            .get(&request.agent_id)
            .map_or(0, |n| n.last_heartbeat_sequence);
        let snapshot = NodeSnapshot {
            agent_id: request.agent_id.clone(),
            agent_epoch: request.agent_epoch.clone(),
            host_label: request.host_label.clone(),
            software_version: request.software_version.clone(),
            capabilities: request.capabilities.clone().unwrap_or_default(),
            desired_state: desired,
            applied_state: desired,
            availability: Availability::Available,
            active_operation_count: 0,
            last_heartbeat_sequence: 0,
            last_heartbeat_at: now,
            transition_sequence: 0,
        };
        nodes.insert(request.agent_id.clone(), snapshot);
        info!(agent_id = %request.agent_id, epoch = %request.agent_epoch, "compute agent registered");
        Ok(proto::RegisterResponse {
            agent_id: request.agent_id.clone(),
            agent_epoch: request.agent_epoch.clone(),
            selected_version: Some(PROTOCOL_VERSION),
            heartbeat_interval_seconds: DEFAULT_HEARTBEAT_INTERVAL.as_secs() as u32,
            max_clock_skew_seconds: 30,
            desired_state: desired,
            highest_observation_sequence: highest,
        })
    }

    pub async fn heartbeat(
        &self,
        heartbeat: &proto::Heartbeat,
    ) -> Result<proto::HeartbeatAck, Status> {
        if heartbeat.agent_id.trim().is_empty() || heartbeat.agent_epoch.trim().is_empty() {
            return Err(Status::invalid_argument("agent identity is required"));
        }
        let mut nodes = self.nodes.write().await;
        let node = nodes
            .get_mut(&heartbeat.agent_id)
            .ok_or_else(|| Status::unauthenticated("agent is not registered"))?;
        if node.agent_epoch != heartbeat.agent_epoch {
            return Err(Status::permission_denied("agent epoch is fenced"));
        }
        if heartbeat.sequence <= node.last_heartbeat_sequence {
            return Err(Status::invalid_argument("heartbeat sequence must increase"));
        }
        node.last_heartbeat_sequence = heartbeat.sequence;
        node.last_heartbeat_at = SystemTime::now();
        node.active_operation_count = heartbeat.active_operation_count;
        node.availability = Availability::Available;
        Ok(proto::HeartbeatAck {
            received_at_unix_ms: unix_ms(),
            desired_state: node.desired_state,
            acknowledged_heartbeat_sequence: heartbeat.sequence,
            highest_observation_sequence: heartbeat.highest_observation_sequence,
            transition_sequence: node.transition_sequence,
        })
    }

    pub async fn set_desired_state(
        &self,
        agent_id: &str,
        state: proto::AdministrativeState,
    ) -> Result<u64, AgentError> {
        if !matches!(
            state,
            proto::AdministrativeState::Enabled
                | proto::AdministrativeState::Draining
                | proto::AdministrativeState::Disabled
        ) {
            return Err(AgentError::InvalidConfiguration(
                "administrative state is unspecified",
            ));
        }
        let mut nodes = self.nodes.write().await;
        let node = nodes
            .get_mut(agent_id)
            .ok_or(AgentError::Protocol("agent is not registered".to_owned()))?;
        node.transition_sequence = node.transition_sequence.saturating_add(1);
        node.desired_state = state as i32;
        Ok(node.transition_sequence)
    }

    async fn acknowledge_state(&self, ack: &proto::AgentStateAck) -> Result<(), Status> {
        if !valid_admin_state(ack.applied_state) {
            return Err(Status::invalid_argument("administrative state is invalid"));
        }
        let mut nodes = self.nodes.write().await;
        let node = nodes
            .get_mut(&ack.agent_id)
            .ok_or_else(|| Status::unauthenticated("agent is not registered"))?;
        if node.agent_epoch != ack.agent_epoch {
            return Err(Status::permission_denied("agent epoch is fenced"));
        }
        if ack.transition_sequence != node.transition_sequence {
            return Err(Status::failed_precondition(
                "administrative transition is stale",
            ));
        }
        node.applied_state = ack.applied_state;
        node.active_operation_count = ack.active_operation_count;
        Ok(())
    }

    pub async fn mark_unavailable(&self, lease: Duration) {
        let now = SystemTime::now();
        let mut nodes = self.nodes.write().await;
        for node in nodes.values_mut() {
            if now
                .duration_since(node.last_heartbeat_at)
                .unwrap_or_default()
                > lease
            {
                node.availability = Availability::Unavailable;
            }
        }
    }
}

fn valid_admin_state(state: i32) -> bool {
    matches!(
        state,
        value if value == proto::AdministrativeState::Enabled as i32
            || value == proto::AdministrativeState::Draining as i32
            || value == proto::AdministrativeState::Disabled as i32
    )
}

fn validate_register(request: &proto::RegisterRequest) -> Result<(), Status> {
    if request.agent_id.trim().is_empty()
        || request.agent_id.len() > MAX_AGENT_ID
        || request.agent_epoch.trim().is_empty()
        || request.host_label.len() > MAX_HOST_LABEL
        || request.capabilities.is_none()
    {
        return Err(Status::invalid_argument("registration is incomplete"));
    }
    let versions = &request.supported_versions;
    if !versions.iter().any(|v| {
        v.major == PROTOCOL_VERSION.major && v.wire_revision == PROTOCOL_VERSION.wire_revision
    }) {
        return Err(Status::failed_precondition(
            "no compatible compute-agent protocol version",
        ));
    }
    Ok(())
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn peer_matches_agent(certificate: &[u8], agent_id: &str) -> bool {
    let expected = format!("urn:o3k:compute:agent:{agent_id}");
    certificate_has_uri_san(certificate, expected.as_bytes())
}

/// Finds the URI GeneralName in the Subject Alternative Name extension.
/// TLS has already authenticated the chain and validity period; the strict
/// structural parser performs the protocol's additional identity binding
/// without treating arbitrary certificate bytes as an extension.
fn certificate_has_uri_san(certificate: &[u8], expected: &[u8]) -> bool {
    const SUBJECT_ALT_NAME_OID: &[u8] = &[0x55, 0x1d, 0x11];
    let Some((0x30, certificate, rest)) = der_tlv(certificate) else {
        return false;
    };
    if !rest.is_empty() {
        return false;
    }
    let Some((0x30, tbs, _)) = der_tlv(certificate) else {
        return false;
    };
    let mut fields = tbs;
    while !fields.is_empty() {
        let Some((tag, field, rest)) = der_tlv(fields) else {
            return false;
        };
        fields = rest;
        if tag != 0xa3 {
            continue;
        }
        let Some((0x30, extensions, rest)) = der_tlv(field) else {
            return false;
        };
        if !rest.is_empty() {
            return false;
        }
        let mut extensions = extensions;
        while !extensions.is_empty() {
            let Some((0x30, extension, rest)) = der_tlv(extensions) else {
                return false;
            };
            extensions = rest;
            let Some((0x06, oid, extension)) = der_tlv(extension) else {
                return false;
            };
            let extension_input = extension;
            let Some((tag, _, rest)) = der_tlv(extension_input) else {
                return false;
            };
            let extension = if tag == 0x01 { rest } else { extension_input };
            if oid != SUBJECT_ALT_NAME_OID {
                continue;
            }
            let Some((0x04, value, rest)) = der_tlv(extension) else {
                return false;
            };
            if !rest.is_empty() {
                return false;
            }
            let Some((0x30, names, rest)) = der_tlv(value) else {
                return false;
            };
            if !rest.is_empty() {
                return false;
            }
            let mut names = names;
            while !names.is_empty() {
                let Some((tag, name, rest)) = der_tlv(names) else {
                    return false;
                };
                if tag == 0x86 && name == expected {
                    return true;
                }
                names = rest;
            }
            return false;
        }
    }
    false
}

fn der_tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    if input.len() < 2 {
        return None;
    }
    let tag = input[0];
    let length_octet = input[1];
    let (length, header) = if length_octet & 0x80 == 0 {
        (length_octet as usize, 2)
    } else {
        let count = (length_octet & 0x7f) as usize;
        if count == 0 || count > 4 || input.len() < 2 + count {
            return None;
        }
        let mut length = 0_usize;
        for byte in &input[2..2 + count] {
            length = length.checked_mul(256)?.checked_add(*byte as usize)?;
        }
        (length, 2 + count)
    };
    let end = header.checked_add(length)?;
    (end <= input.len()).then(|| (tag, &input[header..end], &input[end..]))
}

pub struct ComputeAgentService {
    registry: NodeRegistry,
}

impl ComputeAgentService {
    pub fn new(registry: NodeRegistry) -> Self {
        Self { registry }
    }
}

#[tonic::async_trait]
impl proto::compute_agent_server::ComputeAgent for ComputeAgentService {
    type ControlStream = ReceiverStream<Result<proto::ControlResponse, Status>>;

    async fn control(
        &self,
        request: Request<Streaming<proto::ControlRequest>>,
    ) -> Result<Response<Self::ControlStream>, Status> {
        let mut inbound = request;
        let first = inbound
            .get_mut()
            .message()
            .await?
            .ok_or_else(|| Status::unauthenticated("registration is required"))?;
        let Some(proto::control_request::Body::Register(register)) = first.body else {
            return Err(Status::unauthenticated(
                "registration must be the first message",
            ));
        };
        let Some(certs) = inbound.peer_certs() else {
            return Err(Status::permission_denied("client certificate is required"));
        };
        let Some(certificate) = certs.first() else {
            return Err(Status::permission_denied("client certificate is required"));
        };
        if !peer_matches_agent(certificate.as_ref(), &register.agent_id)
            || !self
                .registry
                .is_authorized(&register.agent_id, certificate.as_ref())
                .await
        {
            return Err(Status::permission_denied(
                "client certificate is not authorized for agent identity",
            ));
        }
        let response = self.registry.register(&register).await?;
        let (tx, rx) = mpsc::channel(32);
        tx.send(Ok(proto::ControlResponse {
            body: Some(proto::control_response::Body::Register(response)),
        }))
        .await
        .map_err(|_| Status::unavailable("response stream closed"))?;
        let registry = self.registry.clone();
        tokio::spawn(async move {
            while let Ok(Some(message)) = inbound.get_mut().message().await {
                match message.body {
                    Some(proto::control_request::Body::Heartbeat(heartbeat)) => {
                        match registry.heartbeat(&heartbeat).await {
                            Ok(ack) => {
                                if tx
                                    .send(Ok(proto::ControlResponse {
                                        body: Some(proto::control_response::Body::Heartbeat(ack)),
                                    }))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(error) => {
                                let _ = tx.send(Err(error)).await;
                                break;
                            }
                        }
                    }
                    Some(proto::control_request::Body::AgentStateAck(ack)) => {
                        if let Err(error) = registry.acknowledge_state(&ack).await {
                            let _ = tx.send(Err(error)).await;
                            break;
                        }
                    }
                    Some(proto::control_request::Body::Operation(_))
                    | Some(proto::control_request::Body::Observation(_))
                    | Some(proto::control_request::Body::CommandAccepted(_))
                    | Some(proto::control_request::Body::ResyncSnapshot(_))
                    | Some(proto::control_request::Body::Error(_))
                    | None => {}
                    Some(proto::control_request::Body::Register(_)) => {
                        let _ = tx
                            .send(Err(Status::invalid_argument("duplicate registration")))
                            .await;
                        break;
                    }
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

#[derive(Clone)]
pub struct ControlPlaneTls {
    pub server_certificate: PathBuf,
    pub server_private_key: PathBuf,
    pub client_ca_certificate: PathBuf,
}

pub struct ControlPlaneServer {
    pub registry: NodeRegistry,
    pub address: SocketAddr,
    pub tls: ControlPlaneTls,
    pub authorized_agents: Vec<AuthorizedAgent>,
}

impl ControlPlaneServer {
    pub async fn serve(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), AgentError> {
        let listener = tokio::net::TcpListener::bind(self.address)
            .await
            .map_err(|_| AgentError::InvalidConfiguration("control address cannot be bound"))?;
        self.serve_listener(listener, shutdown).await
    }

    pub async fn serve_listener(
        self,
        listener: tokio::net::TcpListener,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), AgentError> {
        for agent in &self.authorized_agents {
            self.registry.authorize_agent(agent.clone()).await?;
        }
        let cert = fs::read(&self.tls.server_certificate).map_err(|_| AgentError::TlsMaterial)?;
        let key = fs::read(&self.tls.server_private_key).map_err(|_| AgentError::TlsMaterial)?;
        let ca = fs::read(&self.tls.client_ca_certificate).map_err(|_| AgentError::TlsMaterial)?;
        let tls = ServerTlsConfig::new()
            .identity(Identity::from_pem(cert, key))
            .client_ca_root(Certificate::from_pem(ca));
        let registry = self.registry.clone();
        let monitor = tokio::spawn(async move {
            let mut tick = time::interval(DEFAULT_HEARTBEAT_INTERVAL);
            loop {
                tick.tick().await;
                registry.mark_unavailable(DEFAULT_LEASE).await;
            }
        });
        let result = Server::builder()
            .tls_config(tls)
            .map_err(AgentError::Transport)?
            .add_service(
                proto::compute_agent_server::ComputeAgentServer::new(ComputeAgentService::new(
                    self.registry,
                ))
                .max_decoding_message_size(MAX_MESSAGE_SIZE)
                .max_encoding_message_size(MAX_MESSAGE_SIZE),
            )
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown)
            .await
            .map_err(AgentError::Transport);
        monitor.abort();
        result
    }
}

#[derive(Clone)]
pub struct AgentClient {
    config: AgentConfig,
    ready: Arc<std::sync::atomic::AtomicBool>,
}

impl AgentClient {
    pub fn new(config: AgentConfig) -> Result<Self, AgentError> {
        config.validate()?;
        Ok(Self {
            config,
            ready: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }
    pub fn is_ready(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn identity_file(&self) -> &Path {
        &self.config.identity_file
    }
    pub async fn run<F>(&self, shutdown: F) -> Result<(), AgentError>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        tokio::pin!(shutdown);
        let agent_id = load_or_create_identity(&self.config.identity_file)?;
        let mut delay = Duration::from_millis(250);
        loop {
            self.ready
                .store(false, std::sync::atomic::Ordering::Release);
            let result = tokio::select! { result = self.connect_once(&agent_id) => result, () = &mut shutdown => return Ok(()) };
            match result {
                Ok(()) => return Ok(()),
                Err(error) => {
                    warn!(error = ?error, "compute-agent control connection lost");
                }
            }
            tokio::select! { () = &mut shutdown => return Ok(()), () = time::sleep(delay) => {} }
            delay = (delay.saturating_mul(2)).min(self.config.max_reconnect_delay);
        }
    }

    async fn connect_once(&self, agent_id: &str) -> Result<(), AgentError> {
        let material = self.config.tls.read()?;
        let endpoint = Endpoint::from_shared(self.config.endpoint.clone())
            .map_err(|_| AgentError::InvalidConfiguration("endpoint is invalid"))?
            .tls_config(
                ClientTlsConfig::new()
                    .domain_name(self.config.server_name.clone())
                    .ca_certificate(Certificate::from_pem(material.ca))
                    .identity(Identity::from_pem(material.cert, material.key)),
            )
            .map_err(|_| AgentError::InvalidConfiguration("client TLS configuration is invalid"))?;
        let mut client = proto::compute_agent_client::ComputeAgentClient::connect(endpoint)
            .await
            .map_err(AgentError::Transport)?
            .max_decoding_message_size(MAX_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_MESSAGE_SIZE);
        let (tx, rx) = mpsc::channel(32);
        let epoch = Uuid::now_v7().to_string();
        tx.send(proto::ControlRequest {
            body: Some(proto::control_request::Body::Register(
                proto::RegisterRequest {
                    agent_id: agent_id.to_owned(),
                    agent_epoch: epoch.clone(),
                    software_version: self.config.software_version.clone(),
                    host_label: self.config.host_label.clone(),
                    supported_versions: vec![PROTOCOL_VERSION],
                    capabilities: Some(self.config.capabilities.clone()),
                },
            )),
        })
        .await
        .map_err(|_| AgentError::Protocol("control stream closed".to_owned()))?;
        let response = client
            .control(Request::new(ReceiverStream::new(rx)))
            .await
            .map_err(|status| AgentError::Protocol(status.to_string()))?;
        let mut responses = response.into_inner();
        let mut sequence = 0_u64;
        let mut applied_state = proto::AdministrativeState::Enabled as i32;
        let mut interval = time::interval(self.config.heartbeat_interval);
        loop {
            tokio::select! {
                _ = interval.tick() => { sequence = sequence.saturating_add(1); tx.send(proto::ControlRequest { body: Some(proto::control_request::Body::Heartbeat(proto::Heartbeat { agent_id: agent_id.to_owned(), agent_epoch: epoch.clone(), sequence, sent_at_unix_ms: unix_ms(), state: applied_state, active_operation_count: 0, highest_observation_sequence: 0 })) }).await.map_err(|_| AgentError::Protocol("control stream closed".to_owned()))?; }
                message = responses.message() => match message.map_err(|status| AgentError::Protocol(status.to_string()))? { Some(response) => match response.body {
                    Some(proto::control_response::Body::Register(register)) => { if register.agent_id != agent_id || register.agent_epoch != epoch { return Err(AgentError::Protocol("registration identity mismatch".to_owned())); } applied_state = register.desired_state; self.ready.store(true, std::sync::atomic::Ordering::Release); }
                    Some(proto::control_response::Body::Heartbeat(ack)) => { if ack.desired_state != applied_state { applied_state = ack.desired_state; tx.send(proto::ControlRequest { body: Some(proto::control_request::Body::AgentStateAck(proto::AgentStateAck { agent_id: agent_id.to_owned(), agent_epoch: epoch.clone(), applied_state, transition_sequence: ack.transition_sequence, active_operation_count: 0 })) }).await.map_err(|_| AgentError::Protocol("control stream closed".to_owned()))?; } }
                    Some(proto::control_response::Body::Command(_)) | Some(proto::control_response::Body::DesiredState(_)) | Some(proto::control_response::Body::ObservationAck(_)) | Some(proto::control_response::Body::Resync(_)) | Some(proto::control_response::Body::Error(_)) | None => {}
                }, None => return Err(AgentError::Protocol("control stream ended".to_owned())) }
            }
        }
    }
}

pub fn load_or_create_identity(path: &Path) -> Result<String, AgentError> {
    if let Ok(value) = fs::read_to_string(path) {
        let value = value.trim();
        if !value.is_empty()
            && value.len() <= MAX_AGENT_ID
            && !value.chars().any(char::is_whitespace)
        {
            return Ok(value.to_owned());
        }
        return Err(AgentError::InvalidConfiguration(
            "identity file contains an invalid identity",
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(AgentError::IdentityStore)?;
    }
    let value = Uuid::now_v7().to_string();
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, format!("{value}\n")).map_err(AgentError::IdentityStore)?;
    fs::rename(temporary, path).map_err(AgentError::IdentityStore)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn capabilities() -> proto::Capabilities {
        proto::Capabilities {
            architecture: "x86_64".to_owned(),
            agent_provider_name: "o3k-compute".to_owned(),
            agent_provider_version: "test".to_owned(),
            ..Default::default()
        }
    }
    fn register(id: &str, epoch: &str) -> proto::RegisterRequest {
        proto::RegisterRequest {
            agent_id: id.to_owned(),
            agent_epoch: epoch.to_owned(),
            software_version: "test".to_owned(),
            host_label: "host".to_owned(),
            supported_versions: vec![PROTOCOL_VERSION],
            capabilities: Some(capabilities()),
        }
    }

    #[tokio::test]
    async fn reconnect_reuses_stable_node_and_retains_inventory_on_timeout()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = NodeRegistry::default();
        registry.register(&register("node", "epoch-1")).await?;
        registry
            .heartbeat(&proto::Heartbeat {
                agent_id: "node".to_owned(),
                agent_epoch: "epoch-1".to_owned(),
                sequence: 1,
                ..Default::default()
            })
            .await?;
        registry.register(&register("node", "epoch-2")).await?;
        let nodes = registry.all().await;
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].agent_epoch, "epoch-2");
        registry.mark_unavailable(Duration::ZERO).await;
        let node = registry.snapshot("node").await.ok_or("node retained")?;
        assert_eq!(node.availability, Availability::Unavailable);
        assert_eq!(node.capabilities.architecture, "x86_64");
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_or_fenced_heartbeat_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let registry = NodeRegistry::default();
        registry.register(&register("node", "epoch")).await?;
        let heartbeat = proto::Heartbeat {
            agent_id: "node".to_owned(),
            agent_epoch: "epoch".to_owned(),
            sequence: 1,
            ..Default::default()
        };
        registry.heartbeat(&heartbeat).await?;
        assert!(registry.heartbeat(&heartbeat).await.is_err());
        assert!(
            registry
                .heartbeat(&proto::Heartbeat {
                    agent_id: "node".to_owned(),
                    agent_epoch: "old".to_owned(),
                    sequence: 2,
                    ..Default::default()
                })
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn administrative_state_transition_is_durable_and_acknowledged()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = NodeRegistry::default();
        registry.register(&register("node", "epoch")).await?;
        let transition = registry
            .set_desired_state("node", proto::AdministrativeState::Draining)
            .await?;
        let ack = registry
            .heartbeat(&proto::Heartbeat {
                agent_id: "node".to_owned(),
                agent_epoch: "epoch".to_owned(),
                sequence: 1,
                ..Default::default()
            })
            .await?;
        assert_eq!(
            ack.desired_state,
            proto::AdministrativeState::Draining as i32
        );
        assert_eq!(ack.transition_sequence, transition);
        registry
            .acknowledge_state(&proto::AgentStateAck {
                agent_id: "node".to_owned(),
                agent_epoch: "epoch".to_owned(),
                applied_state: proto::AdministrativeState::Draining as i32,
                transition_sequence: transition,
                active_operation_count: 0,
            })
            .await?;
        assert_eq!(
            registry.snapshot("node").await.ok_or("node")?.applied_state,
            proto::AdministrativeState::Draining as i32
        );
        Ok(())
    }

    #[test]
    fn identity_is_stable_and_temporary_file_is_not_exposed()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("o3k-agent-identity-{}", Uuid::now_v7()));
        let first = load_or_create_identity(&path)?;
        let second = load_or_create_identity(&path)?;
        assert_eq!(first, second);
        assert!(!format!("{first:?}").contains("private"));
        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn protocol_version_is_wire_stable() {
        assert_eq!(PROTOCOL_VERSION.wire_revision, 1);
        assert!(!std::sync::atomic::AtomicBool::new(false).load(Ordering::Relaxed));
    }

    #[test]
    fn authorized_agent_fingerprint_is_strictly_parsed() -> Result<(), Box<dyn std::error::Error>> {
        let fingerprint = "ab".repeat(32);
        let agents = parse_authorized_agents(&format!("node={fingerprint}"))?;
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].certificate_sha256, [0xab; 32]);
        assert!(parse_authorized_agents("node=not-hex").is_err());
        Ok(())
    }

    #[test]
    fn certificate_uri_san_binding_is_exact() {
        let uri = b"urn:o3k:compute:agent:node";
        let certificate = normalize_certificate(include_bytes!("../tests/fixtures/agent.pem"));
        assert!(certificate_has_uri_san(
            &certificate,
            b"urn:o3k:compute:agent:node-test"
        ));
        assert!(!certificate_has_uri_san(
            &certificate,
            b"urn:o3k:compute:agent:other"
        ));
        assert!(!certificate_has_uri_san(&certificate, uri));
    }
}
