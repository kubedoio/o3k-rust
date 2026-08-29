use ed25519_dalek;
use o3k_kernel;
use o3k_service_sdk;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, serde::Deserialize)]
struct ExternalControllerConfigFile {
    controllers: Vec<ExternalControllerConfig>,
}

#[derive(Debug, serde::Deserialize)]
struct ExternalControllerConfig {
    service_id: String,
    namespace: String,
    endpoint: String,
    server_name: String,
    ca: PathBuf,
    client_certificate: PathBuf,
    client_key: PathBuf,
    principal_id: String,
    principal_name: String,
    manifest_digest: String,
    manifest_generation: u64,
    #[serde(default)]
    delegation_key_id: Option<String>,
    #[serde(default)]
    delegation_signing_key_file: Option<PathBuf>,
}

pub(crate) async fn external_controllers_from_config() -> Result<
    std::collections::BTreeMap<String, std::sync::Arc<o3k_service_sdk::GrpcControllerAdapter>>,
    Box<dyn std::error::Error>,
> {
    let Some(path) = std::env::var_os("O3K_EXTERNAL_CONTROLLER_CONFIG") else {
        return Ok(std::collections::BTreeMap::new());
    };
    let config: ExternalControllerConfigFile = serde_json::from_slice(&std::fs::read(path)?)?;
    let mut controllers = std::collections::BTreeMap::new();
    for entry in config.controllers {
        let tls = o3k_service_sdk::tls::client(
            &entry.ca,
            &entry.client_certificate,
            &entry.client_key,
            &entry.server_name,
        )?;
        let principal = o3k_kernel::ServicePrincipal::new(
            o3k_kernel::PrincipalId::new(entry.principal_id)?,
            entry.principal_name,
            entry.namespace.clone(),
        );
        let controller = o3k_service_sdk::GrpcControllerAdapter::connect(
            &entry.endpoint,
            tls,
            entry.service_id.clone(),
            entry.namespace,
            principal,
            entry.manifest_digest,
            entry.manifest_generation,
        )
        .await?;
        let controller = match (entry.delegation_key_id, entry.delegation_signing_key_file) {
            (Some(key_id), Some(path)) => controller.with_delegation_signer(
                key_id,
                ed25519_dalek::SigningKey::from_bytes(
                    &fs::read(path)?
                        .try_into()
                        .map_err(|_| "delegation signing key must be 32 bytes")?,
                ),
            ),
            (None, None) => controller,
            _ => return Err("delegation key id and key file must be configured together".into()),
        };
        controllers.insert(entry.service_id, std::sync::Arc::new(controller));
    }
    Ok(controllers)
}
