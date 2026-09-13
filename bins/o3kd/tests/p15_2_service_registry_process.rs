//! P15.2 real-process proof: all public service views are projections of the
//! one runtime `ManifestRegistry` authority.
#![allow(clippy::expect_used, clippy::panic)]

use reqwest::Client;
use serde_json::Value;
use std::{
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command},
    time::{Duration, Instant},
};

const PASSWORD: &str = "p15-2-bootstrap-password-not-a-secret";
const SIGNING_KEY: &str = "p15-2-token-signing-key-0123456789abcdef";
const CURSOR_KEY: &str = "p15-2-native-cursor-signing-key-0123456789abcdef";

fn port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("ephemeral port");
    let port = listener.local_addr().expect("local address").port();
    drop(listener);
    port
}

fn spawn(port: u16, data_dir: &PathBuf) -> Child {
    Command::new(env!("CARGO_BIN_EXE_o3kd"))
        .args(["--listen-addr", &format!("127.0.0.1:{port}")])
        .env("O3K_LISTEN_ADDR", format!("127.0.0.1:{port}"))
        .env("O3K_DATA_DIR", data_dir)
        .env("O3K_PROVIDER", "fake")
        .env("O3K_BOOTSTRAP_PASSWORD", PASSWORD)
        .env("O3K_TOKEN_SIGNING_KEY", SIGNING_KEY)
        .env("O3K_NATIVE_CURSOR_HMAC_KEY", CURSOR_KEY)
        .env("O3K_DATABASE_BACKEND", "sqlite")
        .env("O3K_LOG_FILTER", "warn")
        .spawn()
        .expect("spawn o3kd")
}

async fn wait_healthy(client: &Client, base: &str) {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Ok(response) = client.get(format!("{base}/healthz")).send().await
            && response.status().is_success()
        {
            return;
        }
        assert!(Instant::now() < deadline, "o3kd did not become healthy");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn stop(child: &mut Child) {
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().expect("wait") {
            assert!(status.success(), "o3kd exited unsuccessfully: {status}");
            return;
        }
        assert!(Instant::now() < deadline, "o3kd did not stop");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn json(client: &Client, url: String) -> Value {
    client
        .get(url)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json response")
}

#[tokio::test]
async fn p15_2_real_process_views_converge_and_reconstruct() {
    let data_dir = std::env::temp_dir().join(format!("o3k-p15-2-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&data_dir).expect("data directory");
    let port = port();
    let base = format!("http://127.0.0.1:{port}");
    let client = Client::new();
    let mut child = spawn(port, &data_dir);
    wait_healthy(&client, &base).await;

    let services = json(&client, format!("{base}/o3k/v1/services")).await;
    let resource_types = json(&client, format!("{base}/o3k/v1/resource-types")).await;
    let token_response = client
        .post(format!("{base}/v3/auth/tokens"))
        .json(&serde_json::json!({
            "auth": {
                "identity": {"methods": ["password"], "password": {"user": {"name": "admin", "password": PASSWORD}}},
                "scope": {"project": {"name": "admin"}}
            }
        }))
        .send()
        .await
        .expect("token request");
    assert_eq!(token_response.status(), reqwest::StatusCode::CREATED);
    assert!(token_response.headers().get("x-subject-token").is_some());
    let token_body: Value = token_response.json().await.expect("token JSON");

    let service_ids: Vec<&str> = services["services"]
        .as_array()
        .expect("services array")
        .iter()
        .filter_map(|service| service["id"].as_str())
        .collect();
    assert!(service_ids.contains(&"compute"));
    assert!(service_ids.contains(&"network"));
    let volume = services["services"]
        .as_array()
        .expect("services array")
        .iter()
        .find(|service| service["id"] == "volume")
        .expect("volume service");
    assert_eq!(volume["lifecycle_state"], "not_ready");
    assert!(resource_types["resource_types"].is_array());
    let catalog = token_body["token"]["catalog"].as_array().expect("catalog");
    // The configured fake compute/network/image/identity paths are Ready, so
    // their compatibility projections are advertised from the same authority.
    for service_type in ["compute", "network", "image", "identity"] {
        assert!(
            catalog
                .iter()
                .any(|service| service["type"] == service_type)
        );
    }
    // The native volume identity remains discoverable while its unavailable
    // provider keeps it NotReady; the subordinate volumev3 projection cannot
    // make it executable in Keystone.
    assert!(!catalog.iter().any(|service| service["type"] == "volumev3"));

    stop(&mut child).await;
    let mut restarted = spawn(port, &data_dir);
    wait_healthy(&client, &base).await;
    let restarted_services = json(&client, format!("{base}/o3k/v1/services")).await;
    assert_eq!(services, restarted_services);
    stop(&mut restarted).await;
    let _ = std::fs::remove_dir_all(&data_dir);
}
