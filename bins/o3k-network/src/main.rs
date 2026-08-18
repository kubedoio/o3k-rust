mod agent;

use agent::proto::network_agent_server::NetworkAgentServer;
use o3k_domain::NetworkPlanIntent;
use o3k_network::{
    FlatNetworkRealizer, HostNetworkConfig, LinuxRoutedProvider, NetworkAgentIdentity,
    NetworkControllerLease, NetworkPlanExecutor, NetworkPlanRealizer, NodeNetworkPlan,
    PolicyEndpoint, PublicAddressRealizer, RoutedExternalConfig, StatefulPolicyProvider,
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
    let flat = FlatNetworkRealizer::open(
        HostNetworkConfig {
            bridge_name,
            uplink,
        },
        ownership_root,
        dhcp_root,
        dnsmasq,
    )?;
    let routed = match env::var("O3K_NETWORK_EXTERNAL_REALM_ID") {
        Ok(realm) => Some(LinuxRoutedProvider::open(
            RoutedExternalConfig {
                external_realm_id: realm.parse()?,
                uplink: required("O3K_NETWORK_UPLINK")?,
                bridge: required("O3K_NETWORK_BRIDGE")?,
            },
            PathBuf::from(required("O3K_NETWORK_ROUTED_ROOT")?),
        )?),
        Err(_) => None,
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
    let realizer = CompositeRealizer {
        flat,
        routed,
        policy,
        public,
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
}

impl NetworkPlanRealizer for CompositeRealizer {
    type Error = CompositeRealizerError;

    fn realize(&mut self, plan: &NodeNetworkPlan) -> Result<(), Self::Error> {
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
