//! Process entry point for the P14.9 evidence coordinator.
//!
//! This binary owns process control and active readiness probing only.  The
//! migration workflow remains in `MigrationRunner`; a real acceptance adapter
//! must be supplied by the protected test-lab profile before `execute` can
//! produce PASS evidence.

use async_trait::async_trait;
use o3k_migration::acceptance::{
    AcceptanceDriver, AcceptanceEvidence, AcceptanceFailure, AcceptanceHarness, FailureClass,
    GateContext, GateId, GateProof, ReadinessReport,
};
use o3k_migration::acceptance_process::{probe_postgres, probe_toolchain};
use o3k_migration::{
    DiscoveryRequest, EndpointInterface, ReqwestOpenStackSource, ResourceKind, Secret,
    SourceCloudConfig, discover,
};
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, env, path::PathBuf, time::Duration};
use url::Url;

#[derive(Clone)]
struct RuntimeConfig {
    source: SourceCloudConfig,
    source_project_b: String,
    destination: Url,
    destination_token: String,
    database_url: String,
    tofu: String,
    provider: PathBuf,
    provider_version: String,
}

impl RuntimeConfig {
    fn from_env() -> Result<Self, String> {
        let value = |name: &str| env::var(name).map_err(|_| format!("missing {name}"));
        let auth_url = value("O3K_P14_SOURCE_AUTH_URL")?
            .parse()
            .map_err(|_| "invalid O3K_P14_SOURCE_AUTH_URL".to_owned())?;
        let source = SourceCloudConfig {
            cloud_id: value("O3K_P14_SOURCE_CLOUD_ID")?,
            auth_url,
            username: value("O3K_P14_SOURCE_USERNAME")?,
            password: Secret::new(value("O3K_P14_SOURCE_PASSWORD")?)
                .map_err(|error| error.to_string())?,
            user_domain_name: env::var("O3K_P14_SOURCE_USER_DOMAIN")
                .unwrap_or_else(|_| "Default".into()),
            project_id: value("O3K_P14_SOURCE_PROJECT_ID")?,
            region: env::var("O3K_P14_SOURCE_REGION").unwrap_or_else(|_| "RegionOne".into()),
            interface: EndpointInterface::Public,
            allowed_hosts: env::var("O3K_P14_SOURCE_ALLOWED_HOSTS")
                .unwrap_or_default()
                .split(',')
                .filter(|host| !host.trim().is_empty())
                .map(|host| host.trim().to_owned())
                .collect(),
            allow_insecure_tls: env::var("O3K_P14_SOURCE_ALLOW_INSECURE_TLS").as_deref()
                == Ok("true"),
            timeout: Duration::from_secs(15),
        };
        Ok(Self {
            source,
            source_project_b: value("O3K_P14_SOURCE_PROJECT_B")?,
            destination: value("O3K_P14_DESTINATION_URL")?
                .parse()
                .map_err(|_| "invalid O3K_P14_DESTINATION_URL".to_owned())?,
            destination_token: value("O3K_P14_DESTINATION_TOKEN")?,
            database_url: value("O3K_P14_DATABASE_URL")?,
            tofu: env::var("O3K_P14_TOFU").unwrap_or_else(|_| "tofu".into()),
            provider: PathBuf::from(value("O3K_P14_PROVIDER_BINARY")?),
            provider_version: env::var("O3K_P14_PROVIDER_VERSION")
                .unwrap_or_else(|_| "3.4.0".into()),
        })
    }
}

struct RealProbeDriver {
    config: RuntimeConfig,
    readiness: Option<ReadinessReport>,
}

impl RealProbeDriver {
    fn new(config: RuntimeConfig) -> Self {
        Self {
            config,
            readiness: None,
        }
    }

    async fn probe(&mut self) -> ReadinessReport {
        let mut checks = BTreeMap::new();
        let mut refs = Vec::new();
        let source_ok = self.probe_source(&mut refs).await;
        checks.insert("openstack.source_project_a".into(), source_ok);
        let project_b_ok = self.probe_project_b(&mut refs).await;
        checks.insert("openstack.source_project_b".into(), project_b_ok);
        let destination_ok = self.probe_destination(&mut refs).await;
        checks.insert("o3k.destination_apis".into(), destination_ok);
        let postgres_ok = probe_postgres(&self.config.database_url, &mut refs).await;
        checks.insert("postgresql.connection".into(), postgres_ok);
        let (tofu_ok, toolchain) = probe_toolchain(
            &self.config.tofu,
            &self.config.provider,
            &self.config.provider_version,
            &mut refs,
        )
        .await;
        checks.insert("toolchain.pinned".into(), tofu_ok);
        let ready = checks.values().all(|value| *value);
        let diagnostic = if ready {
            "active source, destination, database, and toolchain probes passed".into()
        } else {
            "one or more active P14.9 prerequisites failed; execution is blocked".into()
        };
        ReadinessReport {
            ready,
            checks,
            evidence_refs: refs,
            toolchain,
            diagnostic,
        }
    }

    async fn probe_source(&self, refs: &mut Vec<String>) -> bool {
        let source = match ReqwestOpenStackSource::authenticate(&self.config.source).await {
            Ok(source) => source,
            Err(_) => return false,
        };
        let request = DiscoveryRequest::bounded_default();
        match discover(&source, &self.config.source, &request).await {
            Ok(snapshot) if snapshot.project_id == self.config.source.project_id => {
                refs.push(format!("openstack:project:{}", snapshot.project_id));
                true
            }
            _ => false,
        }
    }

    async fn probe_project_b(&self, refs: &mut Vec<String>) -> bool {
        let mut config = self.config.source.clone();
        config.project_id = self.config.source_project_b.clone();
        let source = match ReqwestOpenStackSource::authenticate(&config).await {
            Ok(source) => source,
            Err(_) => return false,
        };
        let request = DiscoveryRequest {
            selected_kinds: [
                ResourceKind::Project,
                ResourceKind::Network,
                ResourceKind::Server,
                ResourceKind::Volume,
            ]
            .into_iter()
            .collect(),
        };
        match discover(&source, &config, &request).await {
            Ok(snapshot) if snapshot.project_id == self.config.source_project_b => {
                refs.push(format!("openstack:project:{}", snapshot.project_id));
                true
            }
            _ => false,
        }
    }

    async fn probe_destination(&self, refs: &mut Vec<String>) -> bool {
        let client = match Client::builder().timeout(Duration::from_secs(10)).build() {
            Ok(client) => client,
            Err(_) => return false,
        };
        let mut passed = true;
        for path in [
            "/healthz",
            "/readyz",
            "/o3k/v1/compute/servers",
            "/o3k/v1/network/networks",
            "/o3k/v1/volume/volumes",
        ] {
            let mut url = self.config.destination.clone();
            url.set_path(path);
            let result = client
                .get(url)
                .bearer_auth(&self.config.destination_token)
                .send()
                .await;
            if !result.is_ok_and(|response| response.status().is_success()) {
                passed = false;
            }
        }
        if passed {
            refs.push("o3k:active-api-probes".into());
        }
        passed
    }
}

#[async_trait]
impl AcceptanceDriver for RealProbeDriver {
    async fn readiness(&mut self) -> Result<ReadinessReport, AcceptanceFailure> {
        let report = self.probe().await;
        self.readiness = Some(report.clone());
        Ok(report)
    }

    async fn execute_gate(
        &mut self,
        _gate: GateId,
        _context: &GateContext,
    ) -> Result<GateProof, AcceptanceFailure> {
        Err(AcceptanceFailure::Driver {
            class: FailureClass::Environment,
            message: "the protected P14.9 real adapter is not configured; #878 must supply it"
                .into(),
        })
    }

    async fn cleanup(&mut self, _context: &GateContext) -> Result<GateProof, AcceptanceFailure> {
        Err(AcceptanceFailure::Driver {
            class: FailureClass::Environment,
            message: "cleanup adapter is not configured".into(),
        })
    }
}

fn fingerprint(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

fn context(config: &RuntimeConfig) -> GateContext {
    GateContext {
        run_id: uuid::Uuid::new_v4().to_string(),
        migration_id: uuid::Uuid::new_v4().to_string(),
        source_fingerprint: fingerprint(&config.source.cloud_id),
        destination_fingerprint: fingerprint(config.destination.as_str()),
        tested_runtime_head_sha: env::var("O3K_P14_TESTED_RUNTIME_HEAD_SHA")
            .unwrap_or_else(|_| "0".repeat(40)),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = env::args().nth(1).unwrap_or_else(|| "prerequisites".into());
    let output =
        env::var("O3K_P14_9A_EVIDENCE_OUTPUT").unwrap_or_else(|_| "p14-9b-evidence.json".into());
    if mode == "validate" {
        let bytes = std::fs::read(&output)?;
        let evidence: AcceptanceEvidence = serde_json::from_slice(&bytes)?;
        evidence.validate()?;
        println!(
            "P14.9 evidence: {}",
            evidence.execution_result.to_uppercase()
        );
        return Ok(());
    }
    let config = match RuntimeConfig::from_env() {
        Ok(config) => config,
        Err(diagnostic) => {
            let context = GateContext {
                run_id: uuid::Uuid::new_v4().to_string(),
                migration_id: uuid::Uuid::new_v4().to_string(),
                source_fingerprint: "unavailable".into(),
                destination_fingerprint: "unavailable".into(),
                tested_runtime_head_sha: env::var("O3K_P14_TESTED_RUNTIME_HEAD_SHA")
                    .unwrap_or_else(|_| "0".repeat(40)),
            };
            AcceptanceEvidence::blocked(&context, diagnostic).write_json(output.as_ref())?;
            return Ok(());
        }
    };
    if !matches!(
        mode.as_str(),
        "prerequisites" | "execute" | "resume" | "validate" | "cleanup"
    ) {
        return Err(format!("unsupported mode {mode}").into());
    }
    let mut driver = RealProbeDriver::new(config.clone());
    if mode == "prerequisites" {
        let readiness = driver.readiness().await?;
        AcceptanceEvidence::readiness_only(&context(&config), readiness)
            .write_json(output.as_ref())?;
        return Ok(());
    }
    let evidence = AcceptanceHarness::new(driver, context(&config))
        .run()
        .await?;
    evidence.write_json(output.as_ref())?;
    Ok(())
}
