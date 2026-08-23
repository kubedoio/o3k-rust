use o3k_controller_protocol::proto;
use o3k_service_sdk::{ControllerHandler, ExternalControllerClient, ServiceControllerServer};
use std::net::SocketAddr;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

struct Handler;

#[tonic::async_trait]
impl ControllerHandler for Handler {
    async fn health(
        &self,
        _request: proto::HealthRequest,
    ) -> Result<proto::HealthResponse, tonic::Status> {
        Ok(proto::HealthResponse {
            healthy: true,
            detail: "ok".into(),
            protocol_version: Some(proto::Version { major: 1, minor: 0 }),
        })
    }
    async fn capabilities(
        &self,
        _request: proto::CapabilitiesRequest,
    ) -> Result<proto::CapabilitiesResponse, tonic::Status> {
        Ok(proto::CapabilitiesResponse {
            protocol_version: Some(proto::Version { major: 1, minor: 0 }),
            resource_types: vec!["database:instance".into()],
            actions: vec!["database:ReadInstance".into()],
        })
    }
    async fn reconcile(
        &self,
        _request: proto::ReconcileRequest,
    ) -> Result<proto::ReconcileResponse, tonic::Status> {
        Ok(proto::ReconcileResponse {
            accepted: true,
            observation: None,
            failure: None,
        })
    }
    async fn observe(
        &self,
        request: proto::ObserveRequest,
    ) -> Result<proto::ObserveResponse, tonic::Status> {
        Ok(proto::ObserveResponse {
            observation: Some(proto::Observation {
                resource: request.resource,
                exists: true,
                observed_revision: "r1".into(),
                status: br#"{"state":"ready"}"#.to_vec(),
                diagnostics: String::new(),
            }),
            failure: None,
        })
    }
    async fn delete(
        &self,
        _request: proto::DeleteRequest,
    ) -> Result<proto::DeleteResponse, tonic::Status> {
        Ok(proto::DeleteResponse {
            accepted: true,
            observation: None,
            failure: None,
        })
    }
}

#[tokio::test]
async fn real_tonic_mtls_registration_and_observe() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = |name: &str| {
        format!(
            "{}/../../crates/o3k-compute-agent/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        )
    };
    let tls = o3k_service_sdk::tls::server(
        fixture("ca.pem"),
        fixture("server-chain.pem"),
        fixture("server-key.pem"),
    )?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address: SocketAddr = listener.local_addr()?;
    let incoming = TcpListenerStream::new(listener);
    let service =
        ServiceControllerServer::new(Handler, "database", "database", "digest-1", 1).into_service();
    let mut builder = Server::builder().tls_config(tls)?;
    let task = tokio::spawn(async move {
        builder
            .add_service(service)
            .serve_with_incoming(incoming)
            .await
    });
    let client_tls = o3k_service_sdk::tls::client(
        fixture("ca.pem"),
        fixture("agent-chain.pem"),
        fixture("agent-key.pem"),
        "o3k-control-plane",
    )?;
    let mut client =
        ExternalControllerClient::connect(&format!("https://{address}"), client_tls).await?;
    let registered = client
        .register(proto::RegisterRequest {
            service_id: "database".into(),
            namespace: "database".into(),
            manifest_digest: "digest-1".into(),
            manifest_generation: 1,
            supported_versions: vec![proto::Version { major: 1, minor: 0 }],
            ..Default::default()
        })
        .await?;
    let session_id = registered.session_id;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    let context = proto::Context {
        request_id: "req-1".into(),
        operation_id: "op-1".into(),
        action: "database:ReadInstance".into(),
        service_id: "database".into(),
        session_id,
        session_generation: registered.session_generation,
        deadline_unix_ms: now + 60_000,
        replay_identity: "replay-1".into(),
        audit_correlation: "audit-1".into(),
        ..Default::default()
    };
    let response = client
        .observe(proto::ObserveRequest {
            context: Some(context),
            resource: Some(proto::ResourceRef {
                namespace: "database".into(),
                r#type: "instance".into(),
                id: "db-1".into(),
                generation: 1,
            }),
            owner_scope: None,
            delegation: None,
        })
        .await?;
    assert_eq!(
        response
            .observation
            .ok_or("missing observation")?
            .observed_revision,
        "r1"
    );
    task.abort();
    Ok(())
}
