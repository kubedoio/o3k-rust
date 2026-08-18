mod agent;

use agent::proto::network_agent_server::NetworkAgentServer;
use o3k_network::{
    FlatNetworkRealizer, HostNetworkConfig, NetworkAgentIdentity, NetworkControllerLease,
    NetworkPlanExecutor,
};
use std::{env, fs, net::SocketAddr, path::PathBuf};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tracing::info;

fn required(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    env::var(name).map_err(|_| format!("missing required environment variable {name}").into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let agent_id = required("O3K_NETWORK_AGENT_ID")?;
    let agent_epoch = required("O3K_NETWORK_AGENT_EPOCH")?;
    let controller_id = required("O3K_NETWORK_CONTROLLER_ID")?;
    let controller_epoch = required("O3K_NETWORK_CONTROLLER_EPOCH")?;
    let fencing_token = required("O3K_NETWORK_FENCING_TOKEN")?.parse::<u64>()?;
    let root = PathBuf::from(required("O3K_NETWORK_ROOT")?);
    let bridge_name = required("O3K_NETWORK_BRIDGE")?;
    let uplink = env::var("O3K_NETWORK_UPLINK").ok();
    let ownership_root = PathBuf::from(required("O3K_NETWORK_OWNERSHIP_ROOT")?);
    let dhcp_root = PathBuf::from(required("O3K_NETWORK_DHCP_ROOT")?);
    let dnsmasq = PathBuf::from(required("O3K_NETWORK_DNSMASQ")?);
    let address: SocketAddr = required("O3K_NETWORK_LISTEN")?.parse()?;
    let server_cert = fs::read(required("O3K_NETWORK_TLS_CERT")?)?;
    let server_key = fs::read(required("O3K_NETWORK_TLS_KEY")?)?;
    let client_ca = fs::read(required("O3K_NETWORK_TLS_CLIENT_CA")?)?;

    let executor = NetworkPlanExecutor::open(
        root,
        NetworkAgentIdentity {
            agent_id,
            agent_epoch,
        },
        NetworkControllerLease {
            controller_id,
            controller_epoch,
            fencing_token,
        },
    )?;
    let realizer = FlatNetworkRealizer::open(
        HostNetworkConfig {
            bridge_name,
            uplink,
        },
        ownership_root,
        dhcp_root,
        dnsmasq,
    )?;
    let service = agent::NetworkAgentService::new(executor, realizer);
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(server_cert, server_key))
        .client_ca_root(Certificate::from_pem(client_ca));
    let listener = TcpListener::bind(address).await?;
    info!(%address, "o3k-network execution agent listening");
    Server::builder()
        .tls_config(tls)?
        .add_service(NetworkAgentServer::new(service))
        .serve_with_incoming(TcpListenerStream::new(listener))
        .await?;
    Ok(())
}
