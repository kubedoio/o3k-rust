//! Standalone process entry point for the P12.6 conformance controller.
//! Runtime transport configuration is supplied by deployment, never by the
//! ServiceManifest.

use ed25519_dalek::VerifyingKey;
use o3k_controller_protocol::proto::controller_service_server::ControllerServiceServer;
use o3k_database_example::{ChildLifecycleActions, DatabaseControllerHandler};
use o3k_service_sdk::ServiceControllerServer as SdkServer;
use std::{env, net::SocketAddr, path::PathBuf, sync::Arc};

fn required(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    env::var(name).map_err(|_| format!("{name} is required").into())
}

fn verification_key() -> Result<VerifyingKey, Box<dyn std::error::Error>> {
    let bytes: Vec<u8> = required("O3K_DATABASE_DELEGATION_KEY")?
        .split(',')
        .map(str::trim)
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "delegation verification key must contain 32 decimal bytes")?;
    Ok(VerifyingKey::from_bytes(&bytes)?)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listen: SocketAddr = required("O3K_DATABASE_CONTROLLER_LISTEN_ADDR")?.parse()?;
    let composition_endpoint = required("O3K_DATABASE_COMPOSITION_ENDPOINT")?;
    let composition_server_name = required("O3K_DATABASE_COMPOSITION_SERVER_NAME")?;
    let composition_tls = o3k_service_sdk::tls::client(
        PathBuf::from(required("O3K_DATABASE_COMPOSITION_CA")?),
        PathBuf::from(required("O3K_DATABASE_COMPOSITION_CLIENT_CERT")?),
        PathBuf::from(required("O3K_DATABASE_COMPOSITION_CLIENT_KEY")?),
        &composition_server_name,
    )?;
    let composition = Arc::new(
        o3k_service_sdk::composition::GrpcCompositionClient::connect(
            &composition_endpoint,
            composition_tls,
        )
        .await?,
    );
    let lifecycle = ChildLifecycleActions {
        network_create: o3k_kernel::ActionId::new("network", "CreateNetwork")?,
        network_observe: o3k_kernel::ActionId::new("network", "ReadNetwork")?,
        network_delete: o3k_kernel::ActionId::new("network", "DeleteNetwork")?,
        volume_create: o3k_kernel::ActionId::new("volume", "CreateVolume")?,
        volume_observe: o3k_kernel::ActionId::new("volume", "ReadVolume")?,
        volume_delete: o3k_kernel::ActionId::new("volume", "DeleteVolume")?,
        compute_create: o3k_kernel::ActionId::new("compute", "CreateServer")?,
        compute_observe: o3k_kernel::ActionId::new("compute", "ReadServer")?,
        compute_delete: o3k_kernel::ActionId::new("compute", "DeleteServer")?,
    };
    let handler = DatabaseControllerHandler::new(composition, lifecycle);
    let server = SdkServer::new(
        handler,
        required("O3K_DATABASE_SERVICE_ID")?,
        "database",
        required("O3K_DATABASE_MANIFEST_DIGEST")?,
        required("O3K_DATABASE_MANIFEST_GENERATION")?.parse()?,
    )
    .with_service_principal("database-controller")
    .with_delegation_recipient("o3k-composition")
    .with_delegation_keys(std::collections::HashMap::from([(
        required("O3K_DATABASE_DELEGATION_KEY_ID")?,
        verification_key()?,
    )]));
    let server_tls = o3k_service_sdk::tls::server(
        PathBuf::from(required("O3K_DATABASE_CONTROLLER_CLIENT_CA")?),
        PathBuf::from(required("O3K_DATABASE_CONTROLLER_CERT")?),
        PathBuf::from(required("O3K_DATABASE_CONTROLLER_KEY")?),
    )?;
    let service = ControllerServiceServer::new(server);
    tonic::transport::Server::builder()
        .tls_config(server_tls)?
        .add_service(service)
        .serve(listen)
        .await?;
    Ok(())
}
