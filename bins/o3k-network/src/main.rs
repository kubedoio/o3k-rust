mod agent;

use agent::proto::network_agent_server::NetworkAgentServer;
use o3k_domain::NetworkPlanIntent;
use o3k_network::{
    FabricRealizer, FlatNetworkRealizer, HostNetworkConfig, LinuxRoutedProvider,
    NetworkAgentIdentity, NetworkControllerLease, NetworkPlanExecutor, NetworkPlanRealizer,
    NodeNetworkPlan, PolicyEndpoint, PublicAddressRealizer, RoutedExternalConfig,
    StatefulPolicyProvider, TapAccess,
};
use std::{env, fs, net::SocketAddr, path::PathBuf};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tracing::info;
use uuid::Uuid;

fn required(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    env::var(name).map_err(|_| format!("missing required environment variable {name}").into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The workspace intentionally contains dependencies that expose both
    // Rustls provider families.  Select the workspace's explicit `ring`
    // provider before tonic constructs server TLS state; otherwise a
    // process-level provider cannot be inferred reliably from feature
    // unification.
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt::init();
    let agent_id = required("O3K_NETWORK_AGENT_ID")?;
    let agent_epoch = required("O3K_NETWORK_AGENT_EPOCH")?;
    let controller_id = required("O3K_NETWORK_CONTROLLER_ID")?;
    let controller_epoch = required("O3K_NETWORK_CONTROLLER_EPOCH")?;
    let fencing_token = required("O3K_NETWORK_FENCING_TOKEN")?.parse::<u64>()?;
    let root = PathBuf::from(required("O3K_NETWORK_ROOT")?);
    let bridge_name = required("O3K_NETWORK_BRIDGE")?;
    let uplink = env::var("O3K_NETWORK_UPLINK").ok();
    let external_realm = env::var("O3K_NETWORK_EXTERNAL_REALM_ID")
        .ok()
        .map(|value| value.parse::<Uuid>())
        .transpose()?;
    // In routed mode the external uplink is a distinct north/south link for
    // nftables/routing. It must never be enslaved into the tenant bridge by
    // the flat attachment realizer. Flat-only mode retains the historical
    // optional bridge-uplink behavior.
    let flat_uplink = match env::var("O3K_NETWORK_BRIDGE_UPLINK").ok() {
        Some(value) => Some(value),
        None if external_realm.is_none() => uplink.clone(),
        None => None,
    };
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
    let tap_access = match (
        env::var("O3K_NETWORK_TAP_USER")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        env::var("O3K_NETWORK_TAP_GROUP")
            .ok()
            .filter(|value| !value.trim().is_empty()),
    ) {
        (None, None) => None,
        (Some(user), Some(group)) => Some(TapAccess { user, group }),
        _ => {
            return Err(
                "O3K_NETWORK_TAP_USER and O3K_NETWORK_TAP_GROUP must be set together".into(),
            );
        }
    };
    let flat = FlatNetworkRealizer::open_with_tap_access(
        HostNetworkConfig {
            bridge_name,
            uplink: flat_uplink,
        },
        ownership_root,
        dhcp_root,
        dnsmasq,
        tap_access,
    )?;
    let routed = match external_realm {
        Some(realm) => Some(LinuxRoutedProvider::open(
            RoutedExternalConfig {
                external_realm_id: realm,
                uplink: required("O3K_NETWORK_UPLINK")?,
                bridge: required("O3K_NETWORK_BRIDGE")?,
            },
            PathBuf::from(required("O3K_NETWORK_ROUTED_ROOT")?),
        )?),
        None => None,
    };
    let policy = match env::var("O3K_NETWORK_POLICY_ROOT") {
        Ok(root) => Some(StatefulPolicyProvider::open(root)?),
        Err(_) => None,
    };
    let public = match env::var("O3K_NETWORK_PUBLIC_ROOT") {
        Ok(root) => Some(PublicAddressRealizer::open(
            root,
            required("O3K_NETWORK_UPLINK")?,
        )?),
        Err(_) => None,
    };
    let fabric = match env::var("O3K_NETWORK_FABRIC_ROOT") {
        Ok(root) => Some(FabricRealizer::new(o3k_network::LinuxFabricBackend::open(
            o3k_network::LinuxFabricConfig::for_root(root)
                .with_public_uplink(required("O3K_NETWORK_UPLINK")?),
        )?)),
        Err(_) => None,
    };
    let realizer = CompositeRealizer {
        flat,
        routed,
        policy,
        public,
        fabric,
    };
    let service = agent::NetworkAgentService::new(executor, realizer);
    let recovered = service.reconcile_pending()?;
    info!(
        pending = recovered.len(),
        "reconciled pending network plans at startup"
    );
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

struct CompositeRealizer {
    flat: FlatNetworkRealizer,
    routed: Option<LinuxRoutedProvider>,
    policy: Option<StatefulPolicyProvider>,
    public: Option<PublicAddressRealizer>,
    fabric: Option<FabricRealizer<o3k_network::LinuxFabricBackend>>,
}

#[derive(Debug, thiserror::Error)]
enum CompositeRealizerError {
    #[error("flat realization failed: {0}")]
    Flat(#[from] o3k_network::FlatNetworkError),
    #[error("routed realization failed: {0}")]
    Routed(#[from] o3k_network::RoutedNetworkError),
    #[error("policy realization failed: {0}")]
    Policy(#[from] o3k_network::PolicyNetworkError),
    #[error("public address realization failed: {0}")]
    Public(#[from] o3k_network::PublicAddressError),
    #[error("routed intents require O3K_NETWORK_EXTERNAL_REALM_ID configuration")]
    RoutedNotConfigured,
    #[error("policy intents require O3K_NETWORK_POLICY_ROOT configuration")]
    PolicyNotConfigured,
    #[error("public bindings require O3K_NETWORK_PUBLIC_ROOT configuration")]
    PublicNotConfigured,
    #[error("Edge fabric plans require an activated host fabric provider")]
    FabricNotConfigured,
    #[error("Edge fabric realization failed: {0}")]
    Fabric(String),
    #[error("Edge fabric plan contains an intent not yet activated by the Fabric provider")]
    FabricUnsupportedIntent,
}

impl NetworkPlanRealizer for CompositeRealizer {
    type Error = CompositeRealizerError;

    fn realize(&mut self, plan: &NodeNetworkPlan) -> Result<(), Self::Error> {
        if plan.fabric.is_some() {
            if plan.intents.iter().any(|intent| {
                is_routed_intent(intent)
                    || (is_public_intent(intent) && !fabric_public_intents_match(plan))
                    || (is_policy_intent(intent) && !fabric_policy_intents_match(plan))
            }) {
                return Err(CompositeRealizerError::FabricUnsupportedIntent);
            }
            self.fabric
                .as_mut()
                .ok_or(CompositeRealizerError::FabricNotConfigured)?
                .realize(plan)
                .map_err(|error| CompositeRealizerError::Fabric(error.to_string()))?;
            return Ok(());
        }
        let mut flat_plan = plan.clone();
        flat_plan.intents.retain(is_flat_intent);
        self.flat.realize(&flat_plan)?;
        if plan.intents.iter().any(is_routed_intent) {
            self.routed
                .as_mut()
                .ok_or(CompositeRealizerError::RoutedNotConfigured)?
                .apply(&plan.intents)?;
        }
        if plan.intents.iter().any(is_policy_intent) {
            self.policy
                .as_mut()
                .ok_or(CompositeRealizerError::PolicyNotConfigured)?
                .apply(&plan.intents, &policy_endpoints(plan))?;
        }
        if plan.intents.iter().any(is_public_intent) {
            self.public
                .as_mut()
                .ok_or(CompositeRealizerError::PublicNotConfigured)?
                .apply(&plan.intents)?;
        }
        Ok(())
    }

    fn remove(&mut self, plan: &NodeNetworkPlan) -> Result<(), Self::Error> {
        if plan.fabric.is_some() {
            if plan.intents.iter().any(|intent| {
                is_routed_intent(intent)
                    || (is_public_intent(intent) && !fabric_public_intents_match(plan))
                    || (is_policy_intent(intent) && !fabric_policy_intents_match(plan))
            }) {
                return Err(CompositeRealizerError::FabricUnsupportedIntent);
            }
            self.fabric
                .as_mut()
                .ok_or(CompositeRealizerError::FabricNotConfigured)?
                .remove(plan)
                .map_err(|error| CompositeRealizerError::Fabric(error.to_string()))?;
            return Ok(());
        }
        if plan.intents.iter().any(is_public_intent) {
            self.public
                .as_mut()
                .ok_or(CompositeRealizerError::PublicNotConfigured)?
                .remove_for_plan(&plan.intents)?;
        }
        if plan.intents.iter().any(is_policy_intent) {
            self.policy
                .as_mut()
                .ok_or(CompositeRealizerError::PolicyNotConfigured)?
                .remove_for_plan(&plan.intents, &policy_endpoints(plan))?;
        }
        if plan.intents.iter().any(is_routed_intent) {
            self.routed
                .as_mut()
                .ok_or(CompositeRealizerError::RoutedNotConfigured)?
                .remove()?;
        }
        let mut flat_plan = plan.clone();
        flat_plan.intents.retain(is_flat_intent);
        self.flat.remove(&flat_plan)?;
        Ok(())
    }

    fn observe(&mut self, plan: &NodeNetworkPlan) -> Result<bool, Self::Error> {
        if plan.fabric.is_some() {
            return self
                .fabric
                .as_mut()
                .ok_or(CompositeRealizerError::FabricNotConfigured)?
                .observe(plan)
                .map_err(|error| CompositeRealizerError::Fabric(error.to_string()));
        }
        let mut flat_plan = plan.clone();
        flat_plan.intents.retain(is_flat_intent);
        if !self.flat.observe(&flat_plan)? {
            return Ok(false);
        }
        let mut healthy = true;
        if plan.intents.iter().any(is_routed_intent) {
            healthy &= self
                .routed
                .as_ref()
                .ok_or(CompositeRealizerError::RoutedNotConfigured)
                .and_then(|provider| provider.observe().map_err(Into::into))?;
        }
        if plan.intents.iter().any(is_policy_intent) {
            healthy &= self
                .policy
                .as_ref()
                .ok_or(CompositeRealizerError::PolicyNotConfigured)
                .and_then(|provider| provider.observe().map_err(Into::into))?;
        }
        if plan.intents.iter().any(is_public_intent) {
            healthy &= self
                .public
                .as_ref()
                .ok_or(CompositeRealizerError::PublicNotConfigured)
                .and_then(|provider| provider.observe().map_err(Into::into))?;
        }
        Ok(healthy)
    }
}

fn is_flat_intent(intent: &NetworkPlanIntent) -> bool {
    matches!(
        intent,
        NetworkPlanIntent::AddressRealm { .. }
            | NetworkPlanIntent::EndpointAttachment { .. }
            | NetworkPlanIntent::AddressAssignment { .. }
    )
}

fn is_routed_intent(intent: &NetworkPlanIntent) -> bool {
    matches!(
        intent,
        NetworkPlanIntent::Route(_) | NetworkPlanIntent::Gateway(_) | NetworkPlanIntent::Egress(_)
    )
}

fn is_policy_intent(intent: &NetworkPlanIntent) -> bool {
    matches!(intent, NetworkPlanIntent::Policy(_))
}

fn fabric_policy_intents_match(plan: &NodeNetworkPlan) -> bool {
    let Some(fabric) = &plan.fabric else {
        return false;
    };
    let policies = plan
        .intents
        .iter()
        .filter_map(|intent| match intent {
            NetworkPlanIntent::Policy(policy) => Some(policy.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    policies == fabric.policies
}

fn fabric_public_intents_match(plan: &NodeNetworkPlan) -> bool {
    let Some(fabric) = &plan.fabric else {
        return false;
    };
    let bindings = plan
        .intents
        .iter()
        .filter_map(|intent| match intent {
            NetworkPlanIntent::PublicAddressBinding(binding) => Some(binding.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    bindings == fabric.public_bindings
}

fn is_public_intent(intent: &NetworkPlanIntent) -> bool {
    matches!(intent, NetworkPlanIntent::PublicAddressBinding(_))
}

fn policy_endpoints(plan: &NodeNetworkPlan) -> Vec<PolicyEndpoint> {
    plan.intents
        .iter()
        .filter_map(|intent| match intent {
            NetworkPlanIntent::AddressAssignment {
                endpoint_id,
                address,
                ..
            } => Some(PolicyEndpoint {
                endpoint_id: *endpoint_id,
                address: *address,
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod transport_tests {
    use super::*;
    use std::collections::BTreeMap;
    use uuid::Uuid;

    struct NoopRealizer;

    impl NetworkPlanRealizer for NoopRealizer {
        type Error = std::convert::Infallible;

        fn realize(&mut self, _plan: &NodeNetworkPlan) -> Result<(), Self::Error> {
            Ok(())
        }

        fn remove(&mut self, _plan: &NodeNetworkPlan) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/o3k-compute-agent/tests/fixtures")
            .join(name)
    }

    #[tokio::test]
    async fn m_tls_client_dispatches_a_fenced_plan_to_the_executor()
    -> Result<(), Box<dyn std::error::Error>> {
        // The workspace also uses rustls through sqlx and tonic.  Those
        // integrations do not guarantee that a process-level provider has
        // been selected before this binary-only test constructs TLS state.
        // Install the explicitly configured provider so the test is
        // independent of test ordering.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let root = std::env::temp_dir().join(format!("o3k-network-transport-{}", Uuid::now_v7()));
        let executor = NetworkPlanExecutor::open(
            &root,
            NetworkAgentIdentity {
                agent_id: "agent-transport".to_owned(),
                agent_epoch: "epoch-1".to_owned(),
            },
            NetworkControllerLease {
                controller_id: "controller-transport".to_owned(),
                controller_epoch: "epoch-1".to_owned(),
                fencing_token: 1,
            },
        )?;
        let service = agent::NetworkAgentService::new(executor, NoopRealizer);
        let tls = ServerTlsConfig::new()
            .identity(Identity::from_pem(
                fs::read(fixture("server-chain.pem"))?,
                fs::read(fixture("server-key.pem"))?,
            ))
            .client_ca_root(Certificate::from_pem(fs::read(fixture("ca.pem"))?));
        let server_task = tokio::spawn(async move {
            Server::builder()
                .tls_config(tls)
                .expect("tls")
                .add_service(NetworkAgentServer::new(service))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
        });
        let operation_id = Uuid::now_v7();
        let deadline_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64
            + 60_000;
        let mut plan = NodeNetworkPlan {
            schema_version: 1,
            plan_id: Uuid::now_v7(),
            node_id: "agent-transport".to_owned(),
            operation_id,
            deadline_unix_ms,
            resource_generations: BTreeMap::new(),
            intents: Vec::new(),
            fabric: None,
            fingerprint_sha256: String::new(),
        };
        plan.fingerprint_sha256 = o3k_network::canonical_plan_fingerprint(&plan)?;
        let client = o3k_network_protocol::NetworkAgentClient::connect(
            &format!("https://{address}"),
            "o3k-control-plane",
            fixture("ca.pem"),
            fixture("agent-chain.pem"),
            fixture("agent-key-pkcs8.pem"),
        )
        .await?;
        let result = client
            .execute(
                agent::proto::Register {
                    agent_id: "agent-transport".to_owned(),
                    agent_epoch: "epoch-1".to_owned(),
                },
                agent::proto::NetworkCommand {
                    command_id: Uuid::now_v7().to_string(),
                    operation_id: operation_id.to_string(),
                    idempotency_key: "transport-test".to_owned(),
                    agent_id: "agent-transport".to_owned(),
                    agent_epoch: "epoch-1".to_owned(),
                    controller_id: "controller-transport".to_owned(),
                    controller_epoch: "epoch-1".to_owned(),
                    fencing_token: 1,
                    deadline_unix_ms,
                    plan_json: serde_json::to_string(&plan)?,
                    remove: false,
                },
            )
            .await?;
        assert_eq!(result.status, "succeeded");
        assert!(!result.replayed);
        server_task.abort();
        let _ = server_task.await;
        let _ = fs::remove_dir_all(root);
        Ok(())
    }
}
