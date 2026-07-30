use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .json()
        .init();

    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
    let listener = TcpListener::bind(address).await?;
    info!(%address, "o3kd listening");

    axum::serve(listener, o3k_api::router()).await?;
    Ok(())
}
