//! Provider-independent execution boundary for canonical multi-Realm gateways.
//!
//! `NamespacedRoutedFabricPlan` remains a one-Realm fabric plan.  This module
//! owns the separate execution unit for an L3 gateway and deliberately keeps
//! provider names (Linux namespaces, links, and routing tables) out of the
//! semantic plan.

use o3k_domain::{Ipv4Prefix, L3GatewayExecutionAttachment, L3GatewayExecutionPlan};
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Write},
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum L3GatewayError {
    #[error("gateway execution plan is invalid")]
    InvalidPlan,
    #[error("gateway execution generation is stale")]
    StaleGeneration,
    #[error("gateway execution backend failed: {0}")]
    Backend(String),
    #[error("gateway execution plan serialization failed")]
    Serialization,
}

/// A provider-owned Realm context. It is derived from the Realm fabric
/// provider and is never persisted as canonical L3Gateway desired state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealmExecutionContext {
    pub realm_id: Uuid,
    pub realm_generation: u64,
    pub namespace: String,
    pub bridge: String,
    /// Provider-local Realm interface used for routes from the Realm into a
    /// gateway. It is derived from the Realm fabric ownership record.
    pub realm_interface: String,
}

/// Narrow provider seam for a complete gateway snapshot. Providers may
/// rebuild an aggregate physical topology, but must preserve every attachment
/// present in the supplied complete plan.
pub trait L3GatewayBackend {
    fn apply(&mut self, plan: &L3GatewayExecutionPlan) -> Result<(), L3GatewayError>;
    fn remove(&mut self, gateway_id: Uuid, project_id: &str) -> Result<(), L3GatewayError>;
    fn observe(
        &self,
        gateway_id: Uuid,
        project_id: &str,
    ) -> Result<Option<L3GatewayExecutionPlan>, L3GatewayError>;
}

/// Generic realizer used by control-plane reconciliation and by concrete
/// Linux/provider adapters. The backend owns mutation and observation only.
#[derive(Debug)]
pub struct L3GatewayRealizer<B> {
    backend: B,
}

impl<B> L3GatewayRealizer<B> {
    #[must_use]
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }
}

impl<B: L3GatewayBackend> L3GatewayRealizer<B> {
    pub fn apply(&mut self, plan: &L3GatewayExecutionPlan) -> Result<(), L3GatewayError> {
        validate_plan(plan)?;
        self.backend.apply(plan)
    }

    pub fn remove(&mut self, gateway_id: Uuid, project_id: &str) -> Result<(), L3GatewayError> {
        self.backend.remove(gateway_id, project_id)
    }

    pub fn observe(
        &self,
        gateway_id: Uuid,
        project_id: &str,
    ) -> Result<Option<L3GatewayExecutionPlan>, L3GatewayError> {
        self.backend.observe(gateway_id, project_id)
    }
}

/// Portable provider used for execution-boundary and recovery tests. State is
/// keyed by gateway identity, so one gateway update cannot remove another
/// gateway's Realm attachments.
#[derive(Debug, Default)]
pub struct InMemoryL3GatewayBackend {
    current: BTreeMap<Uuid, L3GatewayExecutionPlan>,
}

impl InMemoryL3GatewayBackend {
    #[must_use]
    pub fn current(&self, gateway_id: Uuid) -> Option<&L3GatewayExecutionPlan> {
        self.current.get(&gateway_id)
    }
}

impl L3GatewayBackend for InMemoryL3GatewayBackend {
    fn apply(&mut self, plan: &L3GatewayExecutionPlan) -> Result<(), L3GatewayError> {
        validate_plan(plan)?;
        if let Some(current) = self.current.get(&plan.gateway_id)
            && plan.gateway_generation < current.gateway_generation
        {
            return Err(L3GatewayError::StaleGeneration);
        }
        self.current.insert(plan.gateway_id, plan.clone());
        Ok(())
    }

    fn remove(&mut self, gateway_id: Uuid, project_id: &str) -> Result<(), L3GatewayError> {
        if self
            .current
            .get(&gateway_id)
            .is_some_and(|plan| plan.project_id != project_id)
        {
            return Err(L3GatewayError::Backend(
                "project ownership conflict".to_owned(),
            ));
        }
        self.current.remove(&gateway_id);
        Ok(())
    }

    fn observe(
        &self,
        gateway_id: Uuid,
        project_id: &str,
    ) -> Result<Option<L3GatewayExecutionPlan>, L3GatewayError> {
        let plan = self.current.get(&gateway_id);
        if plan.is_some_and(|plan| plan.project_id != project_id) {
            return Err(L3GatewayError::Backend(
                "project ownership conflict".to_owned(),
            ));
        }
        Ok(plan.cloned())
    }
}

trait LinuxGatewayCommand: Send + Sync {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)>;
    fn run(&self, program: &str, args: &[&str]) -> io::Result<bool>;
    fn supports_gateway_marker(&self) -> bool {
        false
    }

    /// Reads provider-owned state that is visible from the execution
    /// namespace. Test doubles may return `None`; production commands must
    /// return the table comment when the gateway table exists.
    fn gateway_marker(&self, namespace: &str, table: &str) -> io::Result<Option<String>> {
        let _ = (namespace, table);
        Ok(None)
    }
}

struct SystemLinuxGatewayCommand;

impl LinuxGatewayCommand for SystemLinuxGatewayCommand {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)> {
        let output = Command::new(program).args(args).output()?;
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }

    fn run(&self, program: &str, args: &[&str]) -> io::Result<bool> {
        Ok(Command::new(program).args(args).status()?.success())
    }

    fn gateway_marker(&self, namespace: &str, table: &str) -> io::Result<Option<String>> {
        let output = Command::new("ip")
            .args([
                "netns", "exec", namespace, "nft", "list", "table", "ip", table,
            ])
            .output()?;
        if !output.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text
            .split("comment ")
            .nth(1)
            .and_then(|value| value.split('"').nth(1))
            .map(ToOwned::to_owned))
    }

    fn supports_gateway_marker(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LinuxGatewayState {
    plan: L3GatewayExecutionPlan,
    aggregate_fingerprint: String,
}

/// Durable evidence that this provider instance owns an in-flight mutation.
///
/// This is intentionally separate from the realized state file: during a
/// crash window the provider must be able to distinguish its own partial
/// topology from foreign objects without treating deterministic names as
/// ownership evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum LinuxGatewayPendingMutation {
    Apply {
        plan: L3GatewayExecutionPlan,
    },
    Remove {
        gateway_id: Uuid,
        project_id: String,
        /// The exact realized target being withdrawn.  Keeping this
        /// snapshot makes a restart independent of a partially updated
        /// gateway.json; the legacy identity fields remain for decoding
        /// pending records written by older providers.
        #[serde(default)]
        plan: Option<L3GatewayExecutionPlan>,
    },
}

struct GatewayLease {
    /// The kernel releases this advisory lock if the provider is terminated,
    /// so a crash cannot strand a mutation behind a stale sentinel file.
    _file: File,
}

/// Linux implementation of the separate gateway provider boundary.
///
/// The provider owns one namespace per gateway and a durable aggregate marker.
/// Realm namespaces and bridges are supplied through `realm_contexts`; they
/// remain owned by `LinuxFabricBackend` and are never recreated here.
pub struct LinuxL3GatewayProvider {
    root: PathBuf,
    realm_contexts: BTreeMap<Uuid, RealmExecutionContext>,
    command: Arc<dyn LinuxGatewayCommand>,
    state: BTreeMap<Uuid, LinuxGatewayState>,
}

impl LinuxL3GatewayProvider {
    pub fn open(
        root: impl Into<PathBuf>,
        realm_contexts: BTreeMap<Uuid, RealmExecutionContext>,
    ) -> Result<Self, L3GatewayError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|error| L3GatewayError::Backend(error.to_string()))?;
        let state = load_linux_gateway_state(&root.join("gateway.json"))?;
        let provider = Self {
            root,
            realm_contexts,
            command: Arc::new(SystemLinuxGatewayCommand),
            state,
        };
        provider.validate_loaded_state()?;
        Ok(provider)
    }

    #[cfg(test)]
    fn with_command(
        root: impl Into<PathBuf>,
        realm_contexts: BTreeMap<Uuid, RealmExecutionContext>,
        command: Arc<dyn LinuxGatewayCommand>,
    ) -> Result<Self, L3GatewayError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|error| L3GatewayError::Backend(error.to_string()))?;
        let state = load_linux_gateway_state(&root.join("gateway.json"))?;
        let provider = Self {
            root,
            realm_contexts,
            command,
            state,
        };
        provider.validate_loaded_state()?;
        Ok(provider)
    }

    fn namespace(plan: &L3GatewayExecutionPlan) -> String {
        Self::namespace_for_id(plan.gateway_id)
    }

    fn namespace_for_id(gateway_id: Uuid) -> String {
        let digest = Sha256::digest(gateway_id.as_bytes());
        format!(
            "o3k-gw-{:02x}{:02x}{:02x}{:02x}",
            digest[0], digest[1], digest[2], digest[3]
        )
    }

    fn nft_table(plan: &L3GatewayExecutionPlan) -> String {
        let digest = Sha256::digest(plan.gateway_id.as_bytes());
        format!(
            "o3kgw{:02x}{:02x}{:02x}{:02x}",
            digest[0], digest[1], digest[2], digest[3]
        )
    }

    fn nft_marker(plan: &L3GatewayExecutionPlan) -> Result<String, L3GatewayError> {
        Ok(format!(
            "o3k-l3-gateway:{}:{}",
            plan.gateway_id,
            gateway_plan_fingerprint(plan)?
        ))
    }

    fn ensure_nft_marker(
        &self,
        namespace: &str,
        plan: &L3GatewayExecutionPlan,
        already_owned: bool,
        previous_marker: Option<&str>,
    ) -> Result<(), L3GatewayError> {
        let table = Self::nft_table(plan);
        let marker = Self::nft_marker(plan)?;
        if let Some(existing) = self
            .command
            .gateway_marker(namespace, &table)
            .map_err(|error| L3GatewayError::Backend(error.to_string()))?
        {
            if existing == marker {
                return Ok(());
            }
            if !already_owned || previous_marker != Some(existing.as_str()) {
                return Err(L3GatewayError::Backend("foreign gateway table".to_owned()));
            }
            if !self
                .command
                .run(
                    "ip",
                    &[
                        "netns", "exec", namespace, "nft", "delete", "table", "ip", &table,
                    ],
                )
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?
            {
                return Err(L3GatewayError::Backend(
                    "cannot replace foreign gateway table".to_owned(),
                ));
            }
        }
        if !self
            .command
            .run(
                "ip",
                &[
                    "netns",
                    "exec",
                    namespace,
                    "nft",
                    "add",
                    "table",
                    "ip",
                    &table,
                    "{",
                    "comment",
                    &format!("\"{}\"", marker),
                    ";",
                    "}",
                ],
            )
            .map_err(|error| L3GatewayError::Backend(error.to_string()))?
        {
            return Err(L3GatewayError::Backend(
                "cannot create gateway ownership marker".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_loaded_state(&self) -> Result<(), L3GatewayError> {
        for state in self.state.values() {
            validate_plan(&state.plan)?;
            if state.aggregate_fingerprint != gateway_plan_fingerprint(&state.plan)? {
                return Err(L3GatewayError::Backend(
                    "gateway state fingerprint mismatch".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn ensure_namespace(&self, namespace: &str, already_owned: bool) -> Result<(), L3GatewayError> {
        let (exists, _) = self
            .command
            .output("ip", &["netns", "exec", namespace, "true"])
            .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
        if exists && !already_owned {
            return Err(L3GatewayError::Backend(
                "foreign gateway namespace".to_owned(),
            ));
        }
        if !exists
            && !self
                .command
                .run("ip", &["netns", "add", namespace])
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?
        {
            return Err(L3GatewayError::Backend(
                "cannot create gateway namespace".to_owned(),
            ));
        }
        if !self
            .command
            .run(
                "ip",
                &[
                    "netns",
                    "exec",
                    namespace,
                    "sysctl",
                    "-w",
                    "net.ipv4.ip_forward=1",
                ],
            )
            .map_err(|error| L3GatewayError::Backend(error.to_string()))?
        {
            return Err(L3GatewayError::Backend(
                "cannot enable gateway forwarding".to_owned(),
            ));
        }
        Ok(())
    }

    fn ensure_attachment(
        &self,
        namespace: &str,
        gateway_id: Uuid,
        attachment: &L3GatewayExecutionAttachment,
        already_owned: bool,
    ) -> Result<(), L3GatewayError> {
        let context = self
            .realm_contexts
            .get(&attachment.realm_id)
            .ok_or_else(|| L3GatewayError::Backend("missing Realm execution context".to_owned()))?;
        if context.realm_generation != attachment.realm_generation
            || context.namespace.is_empty()
            || context.bridge.is_empty()
            || context.realm_interface.is_empty()
        {
            return Err(L3GatewayError::StaleGeneration);
        }
        let link_plan = L3GatewayExecutionPlan {
            gateway_id,
            project_id: String::new(),
            gateway_generation: 1,
            attachments: Vec::new(),
            external_realm_id: None,
            external_realm_prefix: None,
            external_realm_generation: None,
            enable_snat: false,
        };
        let (gateway_if, realm_if) = link_names(&link_plan, attachment);
        let (exists, _) = self
            .command
            .output("ip", &["link", "show", "dev", &realm_if])
            .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
        if exists && !already_owned {
            return Err(L3GatewayError::Backend(
                "foreign gateway attachment".to_owned(),
            ));
        }
        if !exists {
            let args = [
                "link",
                "add",
                &realm_if,
                "type",
                "veth",
                "peer",
                "name",
                &gateway_if,
            ];
            if !self
                .command
                .run("ip", &args)
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?
                || !self
                    .command
                    .run("ip", &["link", "set", &gateway_if, "netns", namespace])
                    .map_err(|error| L3GatewayError::Backend(error.to_string()))?
                || !self
                    .command
                    .run("ip", &["link", "set", &realm_if, "master", &context.bridge])
                    .map_err(|error| L3GatewayError::Backend(error.to_string()))?
                || !self
                    .command
                    .run("ip", &["link", "set", &realm_if, "up"])
                    .map_err(|error| L3GatewayError::Backend(error.to_string()))?
            {
                return Err(L3GatewayError::Backend(
                    "cannot create gateway attachment".to_owned(),
                ));
            }
        }
        let provider_address = format!(
            "{}/{}",
            provider_link_addresses(gateway_id, attachment.attachment_id).1,
            attachment.realm_prefix.prefix_len
        );
        if !self
            .command
            .run(
                "ip",
                &[
                    "netns",
                    "exec",
                    namespace,
                    "ip",
                    "addr",
                    "replace",
                    &provider_address,
                    "dev",
                    &gateway_if,
                ],
            )
            .map_err(|error| L3GatewayError::Backend(error.to_string()))?
            || !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &context.namespace,
                        "ip",
                        "addr",
                        "replace",
                        &format!(
                            "{}/{}",
                            provider_link_addresses(gateway_id, attachment.attachment_id).0,
                            attachment.realm_prefix.prefix_len
                        ),
                        "dev",
                        &context.realm_interface,
                    ],
                )
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?
            || !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        namespace,
                        "ip",
                        "link",
                        "set",
                        &gateway_if,
                        "up",
                    ],
                )
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?
            || !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &context.namespace,
                        "ip",
                        "addr",
                        "replace",
                        &format!(
                            "{}/{}",
                            attachment.gateway_address, attachment.realm_prefix.prefix_len
                        ),
                        "dev",
                        &context.realm_interface,
                    ],
                )
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?
        {
            return Err(L3GatewayError::Backend(
                "cannot configure gateway attachment".to_owned(),
            ));
        }
        Ok(())
    }

    fn ensure_routes(
        &self,
        namespace: &str,
        gateway_id: Uuid,
        attachments: &[L3GatewayExecutionAttachment],
    ) -> Result<(), L3GatewayError> {
        // In the gateway namespace a destination is owned by its own veth.
        // The previous source-oriented loop could install the same destination
        // repeatedly and leave the final source as the route owner.
        let link_plan = L3GatewayExecutionPlan {
            gateway_id,
            project_id: String::new(),
            gateway_generation: 1,
            attachments: Vec::new(),
            external_realm_id: None,
            external_realm_prefix: None,
            external_realm_generation: None,
            enable_snat: false,
        };
        for destination in attachments {
            let (gateway_if, _) = link_names(&link_plan, destination);
            let prefix = format!(
                "{}/{}",
                destination.realm_prefix.network, destination.realm_prefix.prefix_len
            );
            if !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        namespace,
                        "ip",
                        "route",
                        "replace",
                        &prefix,
                        "via",
                        &provider_link_addresses(gateway_id, destination.attachment_id)
                            .0
                            .to_string(),
                        "dev",
                        &gateway_if,
                    ],
                )
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?
            {
                return Err(L3GatewayError::Backend(
                    "cannot configure gateway route".to_owned(),
                ));
            }
        }
        for source in attachments {
            let source_context = self.realm_contexts.get(&source.realm_id).ok_or_else(|| {
                L3GatewayError::Backend("missing Realm execution context".to_owned())
            })?;
            for destination in attachments {
                if source.realm_id == destination.realm_id {
                    continue;
                }
                let prefix = format!(
                    "{}/{}",
                    destination.realm_prefix.network, destination.realm_prefix.prefix_len
                );
                if !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns",
                            "exec",
                            &source_context.namespace,
                            "ip",
                            "route",
                            "replace",
                            &prefix,
                            "via",
                            &provider_link_addresses(gateway_id, source.attachment_id)
                                .1
                                .to_string(),
                            "dev",
                            &source_context.realm_interface,
                        ],
                    )
                    .map_err(|error| L3GatewayError::Backend(error.to_string()))?
                {
                    return Err(L3GatewayError::Backend(
                        "cannot configure Realm gateway route".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn ensure_snat(
        &self,
        namespace: &str,
        plan: &L3GatewayExecutionPlan,
        attachments: &[L3GatewayExecutionAttachment],
    ) -> Result<(), L3GatewayError> {
        if !plan.enable_snat || plan.external_realm_id.is_none() {
            return Ok(());
        }
        let external = attachments
            .iter()
            .find(|attachment| Some(attachment.realm_id) == plan.external_realm_id)
            .ok_or(L3GatewayError::InvalidPlan)?;
        let table = Self::nft_table(plan);
        let marker = Self::nft_marker(plan)?;
        let chain = "postrouting";
        let chain_args = [
            "netns",
            "exec",
            namespace,
            "nft",
            "add",
            "chain",
            "ip",
            &table,
            chain,
            "{",
            "type",
            "nat",
            "hook",
            "postrouting",
            "priority",
            "100",
            ";",
            "policy",
            "accept",
            ";",
            "}",
        ];
        if !self
            .command
            .run("ip", &chain_args)
            .map_err(|error| L3GatewayError::Backend(error.to_string()))?
        {
            let (_, existing) = self
                .command
                .output(
                    "ip",
                    &[
                        "netns", "exec", namespace, "nft", "list", "chain", "ip", &table, chain,
                    ],
                )
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
            if !existing.contains("type nat") {
                return Err(L3GatewayError::Backend(
                    "gateway SNAT chain is foreign".to_owned(),
                ));
            }
        }
        let (_, external_if) = link_names(plan, external);
        for source in attachments {
            if source.realm_id == external.realm_id {
                continue;
            }
            let prefix = format!(
                "{}/{}",
                source.realm_prefix.network, source.realm_prefix.prefix_len
            );
            let (_, existing) = self
                .command
                .output(
                    "ip",
                    &[
                        "netns", "exec", namespace, "nft", "list", "chain", "ip", &table, chain,
                    ],
                )
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
            if existing.contains(&prefix)
                && existing.contains(&external_if)
                && existing.contains("masquerade")
            {
                continue;
            }
            if !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        namespace,
                        "nft",
                        "add",
                        "rule",
                        "ip",
                        &table,
                        chain,
                        "ip",
                        "saddr",
                        &prefix,
                        "oifname",
                        &format!("\"{}\"", external_if),
                        "masquerade",
                        "comment",
                        &marker,
                    ],
                )
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?
            {
                return Err(L3GatewayError::Backend(
                    "cannot configure gateway SNAT".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn remove_snat(&self, plan: &L3GatewayExecutionPlan) -> Result<(), L3GatewayError> {
        if !plan.enable_snat || plan.external_realm_id.is_none() {
            return Ok(());
        }
        let namespace = Self::namespace(plan);
        let table = Self::nft_table(plan);
        let (exists, _) = self
            .command
            .output(
                "ip",
                &[
                    "netns",
                    "exec",
                    &namespace,
                    "nft",
                    "list",
                    "chain",
                    "ip",
                    &table,
                    "postrouting",
                ],
            )
            .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
        if exists
            && !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &namespace,
                        "nft",
                        "delete",
                        "chain",
                        "ip",
                        &table,
                        "postrouting",
                    ],
                )
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?
        {
            return Err(L3GatewayError::Backend(
                "cannot remove gateway SNAT state".to_owned(),
            ));
        }
        Ok(())
    }

    fn execution_attachments(
        plan: &L3GatewayExecutionPlan,
    ) -> Result<Vec<L3GatewayExecutionAttachment>, L3GatewayError> {
        let mut attachments = plan.attachments.clone();
        if let (Some(realm_id), Some(realm_prefix)) =
            (plan.external_realm_id, plan.external_realm_prefix)
        {
            let attachment_id = Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                format!("{}:external:{}", plan.gateway_id, realm_id).as_bytes(),
            );
            let gateway_address = u32::from(realm_prefix.network)
                .checked_add(1)
                .map(Ipv4Addr::from)
                .ok_or(L3GatewayError::InvalidPlan)?;
            attachments.push(L3GatewayExecutionAttachment {
                attachment_id,
                attachment_generation: 1,
                realm_id,
                realm_generation: plan.external_realm_generation.unwrap_or(0),
                realm_prefix,
                gateway_address,
            });
        }
        attachments.sort_by_key(|attachment| attachment.attachment_id);
        Ok(attachments)
    }

    fn remove_attachment_links(
        &self,
        old: &L3GatewayExecutionPlan,
        next: &L3GatewayExecutionPlan,
    ) -> Result<(), L3GatewayError> {
        let old_attachments = Self::execution_attachments(old)?;
        let next_attachments = Self::execution_attachments(next)?;
        for attachment in &old_attachments {
            if next_attachments.iter().any(|current| {
                current.attachment_id == attachment.attachment_id
                    && current.realm_prefix == attachment.realm_prefix
            }) {
                continue;
            }
            let (_, realm_if) = link_names(old, attachment);
            let (exists, _) = self
                .command
                .output("ip", &["link", "show", "dev", &realm_if])
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
            if exists
                && !self
                    .command
                    .run("ip", &["link", "del", "dev", &realm_if])
                    .map_err(|error| L3GatewayError::Backend(error.to_string()))?
            {
                return Err(L3GatewayError::Backend(
                    "cannot remove gateway attachment".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn remove_routes(
        &self,
        old: &L3GatewayExecutionPlan,
        next: &L3GatewayExecutionPlan,
    ) -> Result<(), L3GatewayError> {
        let old_attachments = Self::execution_attachments(old)?;
        let next_attachments = Self::execution_attachments(next)?;
        let link_plan = L3GatewayExecutionPlan {
            gateway_id: old.gateway_id,
            project_id: String::new(),
            gateway_generation: 1,
            attachments: Vec::new(),
            external_realm_id: None,
            external_realm_prefix: None,
            external_realm_generation: None,
            enable_snat: false,
        };
        for destination in &old_attachments {
            if next_attachments.iter().any(|item| {
                item.attachment_id == destination.attachment_id
                    && item.realm_prefix == destination.realm_prefix
            }) {
                continue;
            }
            let (gateway_if, _) = link_names(&link_plan, destination);
            let prefix = format!(
                "{}/{}",
                destination.realm_prefix.network, destination.realm_prefix.prefix_len
            );
            let (route_ok, route_output) = self
                .command
                .output(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &Self::namespace(old),
                        "ip",
                        "route",
                        "show",
                        &prefix,
                    ],
                )
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
            let route_exists = route_ok && !route_output.trim().is_empty();
            if route_exists
                && (!route_output.contains(
                    &provider_link_addresses(old.gateway_id, destination.attachment_id)
                        .0
                        .to_string(),
                ) || !route_output.contains(&gateway_if))
            {
                return Err(L3GatewayError::Backend("foreign gateway route".to_owned()));
            }
            if route_exists
                && !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns",
                            "exec",
                            &Self::namespace(old),
                            "ip",
                            "route",
                            "del",
                            &prefix,
                            "via",
                            &provider_link_addresses(old.gateway_id, destination.attachment_id)
                                .0
                                .to_string(),
                            "dev",
                            &gateway_if,
                        ],
                    )
                    .map_err(|error| L3GatewayError::Backend(error.to_string()))?
            {
                return Err(L3GatewayError::Backend(
                    "cannot remove gateway route".to_owned(),
                ));
            }
        }
        for source in &old_attachments {
            let context = self.realm_contexts.get(&source.realm_id).ok_or_else(|| {
                L3GatewayError::Backend("missing Realm execution context".to_owned())
            })?;
            for destination in &old_attachments {
                if source.realm_id == destination.realm_id
                    || next_attachments.iter().any(|item| {
                        item.realm_id == destination.realm_id
                            && item.realm_prefix == destination.realm_prefix
                    })
                {
                    continue;
                }
                let prefix = format!(
                    "{}/{}",
                    destination.realm_prefix.network, destination.realm_prefix.prefix_len
                );
                let (route_ok, route_output) = self
                    .command
                    .output(
                        "ip",
                        &[
                            "netns",
                            "exec",
                            &context.namespace,
                            "ip",
                            "route",
                            "show",
                            &prefix,
                        ],
                    )
                    .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
                if !route_ok || route_output.trim().is_empty() {
                    continue;
                }
                if !route_output.contains(
                    &provider_link_addresses(old.gateway_id, source.attachment_id)
                        .1
                        .to_string(),
                ) || !route_output.contains(&context.realm_interface)
                {
                    return Err(L3GatewayError::Backend(
                        "foreign Realm gateway route".to_owned(),
                    ));
                }
                if !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns",
                            "exec",
                            &context.namespace,
                            "ip",
                            "route",
                            "del",
                            &prefix,
                            "via",
                            &provider_link_addresses(old.gateway_id, source.attachment_id)
                                .1
                                .to_string(),
                            "dev",
                            &context.realm_interface,
                        ],
                    )
                    .map_err(|error| L3GatewayError::Backend(error.to_string()))?
                {
                    return Err(L3GatewayError::Backend(
                        "cannot remove gateway route".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn persist(&self, state: &BTreeMap<Uuid, LinuxGatewayState>) -> Result<(), L3GatewayError> {
        let bytes = serde_json::to_vec_pretty(state).map_err(|_| L3GatewayError::Serialization)?;
        self.persist_bytes("gateway.json", &bytes)
    }

    fn persist_bytes(&self, name: &str, bytes: &[u8]) -> Result<(), L3GatewayError> {
        let temporary = self.root.join(format!("{name}.tmp"));
        let target = self.root.join(name);
        let mut file =
            File::create(&temporary).map_err(|error| L3GatewayError::Backend(error.to_string()))?;
        file.write_all(bytes)
            .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
        file.sync_all()
            .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
        fs::rename(temporary, target)
            .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
        File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| L3GatewayError::Backend(error.to_string()))
    }

    fn persist_pending(
        &self,
        gateway_id: Uuid,
        pending: &LinuxGatewayPendingMutation,
    ) -> Result<(), L3GatewayError> {
        let mut all = load_linux_gateway_pending(&self.root.join("gateway.pending.json"))?;
        all.insert(gateway_id, pending.clone());
        let bytes = serde_json::to_vec_pretty(&all).map_err(|_| L3GatewayError::Serialization)?;
        self.persist_bytes("gateway.pending.json", &bytes)
    }

    fn clear_pending(&self, gateway_id: Uuid) -> Result<(), L3GatewayError> {
        let mut all = load_linux_gateway_pending(&self.root.join("gateway.pending.json"))?;
        all.remove(&gateway_id);
        if all.is_empty() {
            match fs::remove_file(self.root.join("gateway.pending.json")) {
                Ok(()) => File::open(&self.root)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|error| L3GatewayError::Backend(error.to_string())),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(L3GatewayError::Backend(error.to_string())),
            }
        } else {
            let bytes =
                serde_json::to_vec_pretty(&all).map_err(|_| L3GatewayError::Serialization)?;
            self.persist_bytes("gateway.pending.json", &bytes)
        }
    }

    fn pending(
        &self,
        gateway_id: Uuid,
    ) -> Result<Option<LinuxGatewayPendingMutation>, L3GatewayError> {
        Ok(
            load_linux_gateway_pending(&self.root.join("gateway.pending.json"))?
                .remove(&gateway_id),
        )
    }

    fn acquire_lease(&self) -> Result<GatewayLease, L3GatewayError> {
        let path = self.root.join("gateway.lock");
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
        flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
            if error == rustix::io::Errno::WOULDBLOCK || error == rustix::io::Errno::AGAIN {
                L3GatewayError::Backend("gateway provider is busy".to_owned())
            } else {
                L3GatewayError::Backend(format!("gateway provider lock failed: {error}"))
            }
        })?;
        Ok(GatewayLease { _file: file })
    }

    fn reload_under_lease(&mut self) -> Result<(), L3GatewayError> {
        self.state = load_linux_gateway_state(&self.root.join("gateway.json"))?;
        self.validate_loaded_state()
    }
}

impl L3GatewayBackend for LinuxL3GatewayProvider {
    fn apply(&mut self, plan: &L3GatewayExecutionPlan) -> Result<(), L3GatewayError> {
        let _lease = self.acquire_lease()?;
        self.reload_under_lease()?;
        validate_plan(plan)?;
        if let Some(current) = self.state.get(&plan.gateway_id)
            && (current.plan.gateway_id != plan.gateway_id
                || current.plan.project_id != plan.project_id
                || plan.gateway_generation < current.plan.gateway_generation)
        {
            return Err(L3GatewayError::StaleGeneration);
        }
        let namespace = Self::namespace(plan);
        let target_fingerprint = gateway_plan_fingerprint(plan)?;
        let current = self.state.get(&plan.gateway_id);
        let pending = self.pending(plan.gateway_id)?;
        if matches!(pending, Some(LinuxGatewayPendingMutation::Remove { .. })) {
            return Err(L3GatewayError::Backend(
                "gateway removal is pending; reconcile it before applying a new target".to_owned(),
            ));
        }
        let pending_owns_gateway = matches!(
            &pending,
            Some(LinuxGatewayPendingMutation::Apply { plan: pending_plan })
                if pending_plan.gateway_id == plan.gateway_id
                    && pending_plan.project_id == plan.project_id
                    && pending_plan.gateway_generation == plan.gateway_generation
                    && gateway_plan_fingerprint(pending_plan)? == target_fingerprint
        );
        let already_owned = current.is_some() || pending_owns_gateway;
        if !pending_owns_gateway {
            let (namespace_exists, _) = self
                .command
                .output("ip", &["netns", "exec", &namespace, "true"])
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
            if namespace_exists && current.is_none() {
                return Err(L3GatewayError::Backend(
                    "foreign gateway namespace".to_owned(),
                ));
            }
            let target_attachments = Self::execution_attachments(plan)?;
            let previous = current
                .map(|value| Self::execution_attachments(&value.plan))
                .transpose()?;
            for attachment in &target_attachments {
                let owned = previous.as_ref().is_some_and(|items| {
                    items
                        .iter()
                        .any(|item| item.attachment_id == attachment.attachment_id)
                });
                if !owned {
                    let (_, realm_if) = link_names(plan, attachment);
                    let (link_exists, _) = self
                        .command
                        .output("ip", &["link", "show", "dev", &realm_if])
                        .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
                    if link_exists {
                        return Err(L3GatewayError::Backend(
                            "foreign gateway attachment".to_owned(),
                        ));
                    }
                }
            }
            if current.is_none()
                && self.command.supports_gateway_marker()
                && self
                    .command
                    .gateway_marker(&namespace, &Self::nft_table(plan))
                    .map_err(|error| L3GatewayError::Backend(error.to_string()))?
                    .is_some()
            {
                return Err(L3GatewayError::Backend("foreign gateway table".to_owned()));
            }
        }
        self.persist_pending(
            plan.gateway_id,
            &LinuxGatewayPendingMutation::Apply { plan: plan.clone() },
        )?;
        self.ensure_namespace(&namespace, already_owned)?;
        if let Some(current) = self.state.get(&plan.gateway_id) {
            self.remove_routes(&current.plan, plan)?;
            self.remove_attachment_links(&current.plan, plan)?;
            if current.plan.enable_snat
                && (!plan.enable_snat || current.plan.external_realm_id != plan.external_realm_id)
            {
                self.remove_snat(&current.plan)?;
            }
        }
        let execution_attachments = Self::execution_attachments(plan)?;
        let previous_execution_attachments = current
            .map(|current| Self::execution_attachments(&current.plan))
            .transpose()?;
        for attachment in &execution_attachments {
            self.ensure_attachment(
                &namespace,
                plan.gateway_id,
                attachment,
                already_owned
                    && (pending_owns_gateway
                        || previous_execution_attachments.as_ref().is_some_and(|old| {
                            old.iter()
                                .any(|item| item.attachment_id == attachment.attachment_id)
                        })),
            )?;
        }
        self.ensure_routes(&namespace, plan.gateway_id, &execution_attachments)?;
        let previous_marker = current
            .map(|current| Self::nft_marker(&current.plan))
            .transpose()?;
        self.ensure_nft_marker(&namespace, plan, already_owned, previous_marker.as_deref())?;
        self.ensure_snat(&namespace, plan, &execution_attachments)?;
        let state = LinuxGatewayState {
            plan: plan.clone(),
            aggregate_fingerprint: target_fingerprint,
        };
        let mut next = self.state.clone();
        next.insert(plan.gateway_id, state);
        self.persist(&next)?;
        self.clear_pending(plan.gateway_id)?;
        self.state = next;
        Ok(())
    }

    fn remove(&mut self, gateway_id: Uuid, project_id: &str) -> Result<(), L3GatewayError> {
        let _lease = self.acquire_lease()?;
        self.reload_under_lease()?;
        if self
            .state
            .get(&gateway_id)
            .is_some_and(|state| state.plan.project_id != project_id)
        {
            return Err(L3GatewayError::Backend(
                "gateway ownership conflict".to_owned(),
            ));
        }
        let pending = self.pending(gateway_id)?;
        if let Some(LinuxGatewayPendingMutation::Remove {
            project_id: owner, ..
        }) = &pending
            && owner != project_id
        {
            return Err(L3GatewayError::Backend(
                "gateway ownership conflict".to_owned(),
            ));
        }
        let old = if let Some(LinuxGatewayPendingMutation::Remove {
            plan: Some(plan), ..
        }) = &pending
        {
            Some(LinuxGatewayState {
                plan: plan.clone(),
                aggregate_fingerprint: gateway_plan_fingerprint(plan)?,
            })
        } else if let Some(current) = self.state.get(&gateway_id).cloned() {
            Some(current)
        } else if pending.is_some() {
            return Err(L3GatewayError::Backend(
                "pending gateway removal has no exact target".to_owned(),
            ));
        } else {
            None
        };
        if let Some(old) = old {
            self.persist_pending(
                gateway_id,
                &LinuxGatewayPendingMutation::Remove {
                    gateway_id,
                    project_id: project_id.to_owned(),
                    plan: Some(old.plan.clone()),
                },
            )?;
            let empty = L3GatewayExecutionPlan {
                gateway_id,
                project_id: project_id.to_owned(),
                gateway_generation: old.plan.gateway_generation,
                attachments: Vec::new(),
                external_realm_id: old.plan.external_realm_id,
                external_realm_prefix: old.plan.external_realm_prefix,
                external_realm_generation: old.plan.external_realm_generation,
                enable_snat: old.plan.enable_snat,
            };
            self.remove_routes(&old.plan, &empty)?;
            for attachment in &Self::execution_attachments(&old.plan)? {
                let (_, realm_if) = link_names(&old.plan, attachment);
                let (exists, _) = self
                    .command
                    .output("ip", &["link", "show", "dev", &realm_if])
                    .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
                if exists
                    && !self
                        .command
                        .run("ip", &["link", "del", "dev", &realm_if])
                        .map_err(|error| L3GatewayError::Backend(error.to_string()))?
                {
                    return Err(L3GatewayError::Backend(
                        "cannot remove gateway link".to_owned(),
                    ));
                }
            }
            let namespace = Self::namespace(&old.plan);
            let table = Self::nft_table(&old.plan);
            let (table_exists, _) = self
                .command
                .output(
                    "ip",
                    &[
                        "netns", "exec", &namespace, "nft", "list", "table", "ip", &table,
                    ],
                )
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
            if table_exists
                && !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns", "exec", &namespace, "nft", "delete", "table", "ip", &table,
                        ],
                    )
                    .map_err(|error| L3GatewayError::Backend(error.to_string()))?
            {
                return Err(L3GatewayError::Backend(
                    "cannot remove gateway ownership marker".to_owned(),
                ));
            }
            let (namespace_exists, _) = self
                .command
                .output("ip", &["netns", "exec", &namespace, "true"])
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
            if namespace_exists
                && !self
                    .command
                    .run("ip", &["netns", "delete", &namespace])
                    .map_err(|error| L3GatewayError::Backend(error.to_string()))?
            {
                return Err(L3GatewayError::Backend(
                    "cannot remove gateway namespace".to_owned(),
                ));
            }
            let mut next = self.state.clone();
            next.remove(&gateway_id);
            self.persist(&next)?;
            self.clear_pending(gateway_id)?;
            self.state = next;
        }
        if matches!(pending, Some(LinuxGatewayPendingMutation::Remove { .. })) {
            self.clear_pending(gateway_id)?;
        }
        Ok(())
    }

    fn observe(
        &self,
        gateway_id: Uuid,
        project_id: &str,
    ) -> Result<Option<L3GatewayExecutionPlan>, L3GatewayError> {
        // Observation is deliberately reloaded from provider-owned durable
        // state so a long-lived runtime cannot report a stale in-memory
        // aggregate after another provider instance has completed a rebuild.
        let durable_state = load_linux_gateway_state(&self.root.join("gateway.json"))?;
        let Some(state) = durable_state.get(&gateway_id) else {
            if let Some(LinuxGatewayPendingMutation::Remove {
                project_id: owner,
                plan: Some(plan),
                ..
            }) = load_linux_gateway_pending(&self.root.join("gateway.pending.json"))?
                .get(&gateway_id)
            {
                if owner != project_id {
                    return Err(L3GatewayError::Backend(
                        "gateway ownership conflict".to_owned(),
                    ));
                }
                let namespace = Self::namespace(plan);
                let (namespace_exists, _) = self
                    .command
                    .output("ip", &["netns", "exec", &namespace, "true"])
                    .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
                if namespace_exists {
                    return Err(L3GatewayError::Backend(
                        "gateway removal is not yet observable as absent".to_owned(),
                    ));
                }
                return Ok(None);
            }
            let namespace = Self::namespace_for_id(gateway_id);
            let (namespace_exists, _) = self
                .command
                .output("ip", &["netns", "exec", &namespace, "true"])
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
            if namespace_exists {
                return Err(L3GatewayError::Backend(
                    "unowned gateway namespace is not safely observable as absent".to_owned(),
                ));
            }
            return Ok(None);
        };
        if state.plan.project_id != project_id {
            return Err(L3GatewayError::Backend(
                "gateway ownership conflict".to_owned(),
            ));
        }
        if state.aggregate_fingerprint != gateway_plan_fingerprint(&state.plan)? {
            return Err(L3GatewayError::Backend(
                "gateway observation is corrupt".to_owned(),
            ));
        }
        let namespace = Self::namespace(&state.plan);
        let (namespace_exists, _) = self
            .command
            .output("ip", &["netns", "exec", &namespace, "true"])
            .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
        if !namespace_exists {
            return Err(L3GatewayError::Backend(
                "gateway namespace is absent".to_owned(),
            ));
        }
        for attachment in &Self::execution_attachments(&state.plan)? {
            let (_, realm_if) = link_names(&state.plan, attachment);
            let (link_exists, _) = self
                .command
                .output("ip", &["link", "show", "dev", &realm_if])
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
            if !link_exists {
                return Err(L3GatewayError::Backend(
                    "gateway attachment is absent".to_owned(),
                ));
            }
        }
        if self.command.supports_gateway_marker() {
            let table = Self::nft_table(&state.plan);
            let expected_marker = Self::nft_marker(&state.plan)?;
            let Some(observed_marker) = self
                .command
                .gateway_marker(&namespace, &table)
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?
            else {
                return Err(L3GatewayError::Backend(
                    "gateway ownership marker is not observable".to_owned(),
                ));
            };
            if observed_marker != expected_marker {
                return Err(L3GatewayError::Backend(
                    "gateway ownership marker mismatch".to_owned(),
                ));
            }
            let execution_attachments = Self::execution_attachments(&state.plan)?;
            for destination in &execution_attachments {
                let prefix = format!(
                    "{}/{}",
                    destination.realm_prefix.network, destination.realm_prefix.prefix_len
                );
                let (route_exists, _) = self
                    .command
                    .output(
                        "ip",
                        &["netns", "exec", &namespace, "ip", "route", "show", &prefix],
                    )
                    .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
                if !route_exists {
                    return Err(L3GatewayError::Backend(
                        "gateway route is not observable".to_owned(),
                    ));
                }
                let Some(context) = self.realm_contexts.get(&destination.realm_id) else {
                    return Err(L3GatewayError::Backend(
                        "missing Realm execution context".to_owned(),
                    ));
                };
                for peer in &execution_attachments {
                    if peer.realm_id == destination.realm_id {
                        continue;
                    }
                    let peer_prefix = format!(
                        "{}/{}",
                        peer.realm_prefix.network, peer.realm_prefix.prefix_len
                    );
                    let (route_ok, route_output) = self
                        .command
                        .output(
                            "ip",
                            &[
                                "netns",
                                "exec",
                                &context.namespace,
                                "ip",
                                "route",
                                "show",
                                &peer_prefix,
                            ],
                        )
                        .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
                    if !route_ok || route_output.trim().is_empty() {
                        return Err(L3GatewayError::Backend(
                            "Realm gateway route is not observable".to_owned(),
                        ));
                    }
                }
            }
            let chain_args = [
                "netns",
                "exec",
                &namespace,
                "nft",
                "list",
                "chain",
                "ip",
                &table,
                "postrouting",
            ];
            let (snat_exists, snat_output) = self
                .command
                .output("ip", &chain_args)
                .map_err(|error| L3GatewayError::Backend(error.to_string()))?;
            if state.plan.enable_snat {
                if !snat_exists || !snat_output.contains("masquerade") {
                    return Err(L3GatewayError::Backend(
                        "gateway SNAT state is not observable".to_owned(),
                    ));
                }
            } else if snat_exists {
                return Err(L3GatewayError::Backend(
                    "unexpected gateway SNAT state".to_owned(),
                ));
            }
        }
        Ok(Some(state.plan.clone()))
    }
}

fn load_linux_gateway_state(
    path: &Path,
) -> Result<BTreeMap<Uuid, LinuxGatewayState>, L3GatewayError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| L3GatewayError::Backend("gateway state is corrupt".to_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(L3GatewayError::Backend(error.to_string())),
    }
}

fn load_linux_gateway_pending(
    path: &Path,
) -> Result<BTreeMap<Uuid, LinuxGatewayPendingMutation>, L3GatewayError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| L3GatewayError::Backend("gateway pending state is corrupt".to_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(L3GatewayError::Backend(error.to_string())),
    }
}

pub fn gateway_plan_fingerprint(plan: &L3GatewayExecutionPlan) -> Result<String, L3GatewayError> {
    let mut canonical = plan.clone();
    canonical
        .attachments
        .sort_by_key(|attachment| attachment.attachment_id);
    validate_plan(&canonical)?;
    let bytes = serde_json::to_vec(&canonical).map_err(|_| L3GatewayError::Serialization)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub fn validate_plan(plan: &L3GatewayExecutionPlan) -> Result<(), L3GatewayError> {
    if plan.gateway_id.is_nil()
        || plan.gateway_generation == 0
        || plan
            .attachments
            .windows(2)
            .any(|pair| pair[0].attachment_id >= pair[1].attachment_id)
    {
        return Err(L3GatewayError::InvalidPlan);
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut realms = std::collections::BTreeSet::new();
    let mut prefixes = Vec::new();
    for attachment in &plan.attachments {
        if attachment.attachment_id.is_nil()
            || attachment.realm_id.is_nil()
            || attachment.attachment_generation == 0
            || attachment.realm_generation == 0
            || !ids.insert(attachment.attachment_id)
            || !realms.insert(attachment.realm_id)
            || !attachment.realm_prefix.contains(attachment.gateway_address)
            || attachment.gateway_address == attachment.realm_prefix.network
        {
            return Err(L3GatewayError::InvalidPlan);
        }
        // A single Linux routing namespace cannot safely disambiguate two
        // overlapping destination prefixes. Such Realms remain valid and may
        // use separate gateway/provider contexts, but this bounded gateway
        // profile rejects attaching both to one routing domain.
        if prefixes
            .iter()
            .any(|prefix: &Ipv4Prefix| prefix.overlaps(attachment.realm_prefix))
        {
            return Err(L3GatewayError::InvalidPlan);
        }
        prefixes.push(attachment.realm_prefix);
    }
    if let Some(external) = plan.external_realm_id
        && (external.is_nil() || realms.contains(&external))
    {
        return Err(L3GatewayError::InvalidPlan);
    }
    if plan.external_realm_id.is_some() != plan.external_realm_prefix.is_some() {
        return Err(L3GatewayError::InvalidPlan);
    }
    if plan.external_realm_id.is_some() && plan.external_realm_generation.unwrap_or_default() == 0 {
        return Err(L3GatewayError::InvalidPlan);
    }
    Ok(())
}

/// Converts canonical store records into the separate gateway execution plan.
/// This is the service/compiler boundary; provider-native context is supplied
/// separately by the Realm execution directory.
pub fn compile_l3_gateway_execution_plan(
    gateway: &o3k_store::CanonicalL3GatewayRecord,
    attachments: &[o3k_store::CanonicalL3GatewayAttachmentRecord],
    realms: &BTreeMap<Uuid, o3k_store::CanonicalAddressRealmRecord>,
) -> Result<L3GatewayExecutionPlan, L3GatewayError> {
    if gateway.state != "active" || gateway.generation == 0 || gateway.project_id.is_empty() {
        return Err(L3GatewayError::InvalidPlan);
    }
    let mut execution_attachments = Vec::new();
    for attachment in attachments.iter().filter(|item| item.state == "active") {
        if attachment.project_id != gateway.project_id || attachment.gateway_id != gateway.id {
            return Err(L3GatewayError::InvalidPlan);
        }
        let realm = realms
            .get(&attachment.realm_id)
            .ok_or(L3GatewayError::InvalidPlan)?;
        if realm.project_id != gateway.project_id || realm.state != "active" {
            return Err(L3GatewayError::InvalidPlan);
        }
        let prefix = parse_prefix(&realm.prefix)?;
        let gateway_address = u32::from(prefix.network)
            .checked_add(1)
            .map(Ipv4Addr::from)
            .ok_or(L3GatewayError::InvalidPlan)?;
        execution_attachments.push(L3GatewayExecutionAttachment {
            attachment_id: attachment.id,
            attachment_generation: attachment.generation,
            realm_id: realm.id,
            realm_generation: realm.generation,
            realm_prefix: prefix,
            gateway_address,
        });
    }
    execution_attachments.sort_by_key(|item| item.attachment_id);
    Ok(L3GatewayExecutionPlan {
        gateway_id: gateway.id,
        project_id: gateway.project_id.clone(),
        gateway_generation: gateway.generation,
        attachments: execution_attachments,
        external_realm_id: gateway.external_realm_id,
        external_realm_prefix: gateway
            .external_realm_id
            .and_then(|id| realms.get(&id))
            .map(|realm| parse_prefix(&realm.prefix))
            .transpose()?,
        external_realm_generation: gateway
            .external_realm_id
            .and_then(|id| realms.get(&id))
            .map(|realm| realm.generation),
        enable_snat: gateway.enable_snat,
    })
}

fn parse_prefix(value: &str) -> Result<Ipv4Prefix, L3GatewayError> {
    let (network, prefix_len) = value.split_once('/').ok_or(L3GatewayError::InvalidPlan)?;
    let network = network.parse().map_err(|_| L3GatewayError::InvalidPlan)?;
    let prefix_len = prefix_len
        .parse()
        .map_err(|_| L3GatewayError::InvalidPlan)?;
    Ipv4Prefix::new(network, prefix_len).ok_or(L3GatewayError::InvalidPlan)
}

fn provider_link_addresses(gateway_id: Uuid, attachment_id: Uuid) -> (Ipv4Addr, Ipv4Addr) {
    let mut input = Vec::with_capacity(32);
    input.extend_from_slice(gateway_id.as_bytes());
    input.extend_from_slice(attachment_id.as_bytes());
    let digest = Sha256::digest(input);
    let subnet = u16::from_be_bytes([digest[0], digest[1]]) & 0x3fff;
    let base = u32::from(Ipv4Addr::new(169, 254, 0, 0)) + u32::from(subnet) * 4;
    (Ipv4Addr::from(base + 1), Ipv4Addr::from(base + 2))
}

fn link_names(
    plan: &L3GatewayExecutionPlan,
    attachment: &L3GatewayExecutionAttachment,
) -> (String, String) {
    let mut input = Vec::with_capacity(32);
    input.extend_from_slice(plan.gateway_id.as_bytes());
    input.extend_from_slice(attachment.attachment_id.as_bytes());
    let digest = Sha256::digest(input);
    let suffix = format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7]
    );
    (
        format!("o3kg{}", &suffix[..11]),
        format!("o3kr{}", &suffix[..11]),
    )
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeGatewayCommand {
        calls: Mutex<Vec<(String, Vec<String>)>>,
        namespace: Mutex<bool>,
        link: Mutex<bool>,
        links: Mutex<std::collections::BTreeSet<String>>,
    }

    impl FakeGatewayCommand {
        fn new(namespace: bool, link: bool) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                namespace: Mutex::new(namespace),
                link: Mutex::new(link),
                links: Mutex::new(std::collections::BTreeSet::new()),
            }
        }
    }

    impl LinuxGatewayCommand for FakeGatewayCommand {
        fn output(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)> {
            self.calls.lock().expect("calls").push((
                program.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));
            if args.starts_with(&["netns", "exec"]) {
                return Ok((*self.namespace.lock().expect("namespace"), String::new()));
            }
            if args.starts_with(&["link", "show"]) {
                let known = *self.link.lock().expect("link");
                let named = args
                    .iter()
                    .position(|arg| *arg == "dev")
                    .and_then(|index| args.get(index + 1))
                    .is_some_and(|name| self.links.lock().expect("links").contains(*name));
                return Ok((known || named, String::new()));
            }
            Ok((true, String::new()))
        }

        fn run(&self, program: &str, args: &[&str]) -> io::Result<bool> {
            self.calls.lock().expect("calls").push((
                program.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));
            if args.starts_with(&["netns", "add"]) {
                *self.namespace.lock().expect("namespace") = true;
            }
            if args.starts_with(&["link", "add"]) {
                if let Some(name) = args.get(2) {
                    self.links.lock().expect("links").insert((*name).to_owned());
                }
                if let Some(name) = args.last() {
                    self.links.lock().expect("links").insert((*name).to_owned());
                }
            }
            Ok(true)
        }
    }

    fn plan(gateway_id: Uuid, attachment_id: Uuid) -> L3GatewayExecutionPlan {
        L3GatewayExecutionPlan {
            gateway_id,
            project_id: "project-a".to_owned(),
            gateway_generation: 1,
            attachments: vec![L3GatewayExecutionAttachment {
                attachment_id,
                attachment_generation: 1,
                realm_id: Uuid::from_u128(3),
                realm_generation: 1,
                realm_prefix: Ipv4Prefix::new(Ipv4Addr::new(10, 0, 0, 0), 24).expect("prefix"),
                gateway_address: Ipv4Addr::new(10, 0, 0, 1),
            }],
            external_realm_id: None,
            external_realm_prefix: None,
            external_realm_generation: None,
            enable_snat: false,
        }
    }

    #[test]
    fn gateway_provider_uses_kernel_lock_for_live_writer_exclusion() {
        let root = std::env::temp_dir().join(format!("o3k-gateway-lock-{}", Uuid::now_v7()));
        let first = LinuxL3GatewayProvider::open(&root, BTreeMap::new()).expect("first provider");
        let first_lease = first.acquire_lease().expect("first lease");
        let second = LinuxL3GatewayProvider::open(&root, BTreeMap::new()).expect("second provider");
        assert!(matches!(
            second.acquire_lease(),
            Err(L3GatewayError::Backend(message)) if message == "gateway provider is busy"
        ));
        drop(first_lease);
        assert!(second.acquire_lease().is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn gateway_is_independent_realization_unit() {
        let a = plan(Uuid::from_u128(1), Uuid::from_u128(11));
        let b = plan(Uuid::from_u128(2), Uuid::from_u128(12));
        let mut realizer = L3GatewayRealizer::new(InMemoryL3GatewayBackend::default());
        realizer.apply(&a).expect("apply a");
        realizer.apply(&b).expect("apply b");
        assert_eq!(realizer.backend().current(a.gateway_id), Some(&a));
        assert_eq!(realizer.backend().current(b.gateway_id), Some(&b));
    }

    #[test]
    fn stale_gateway_generation_is_rejected() {
        let mut current = plan(Uuid::from_u128(1), Uuid::from_u128(11));
        current.gateway_generation = 2;
        let stale = plan(current.gateway_id, current.attachments[0].attachment_id);
        let mut realizer = L3GatewayRealizer::new(InMemoryL3GatewayBackend::default());
        realizer.apply(&current).expect("current");
        assert_eq!(realizer.apply(&stale), Err(L3GatewayError::StaleGeneration));
    }

    #[test]
    fn fingerprint_is_stable_for_attachment_order() {
        let mut first = plan(Uuid::from_u128(1), Uuid::from_u128(11));
        let second_attachment = L3GatewayExecutionAttachment {
            attachment_id: Uuid::from_u128(12),
            realm_id: Uuid::from_u128(4),
            realm_generation: 1,
            attachment_generation: 1,
            realm_prefix: Ipv4Prefix::new(Ipv4Addr::new(10, 0, 1, 0), 24).expect("prefix"),
            gateway_address: Ipv4Addr::new(10, 0, 1, 1),
        };
        first.attachments.push(second_attachment.clone());
        let mut second = first.clone();
        second.attachments.reverse();
        assert_eq!(
            gateway_plan_fingerprint(&first).expect("first"),
            gateway_plan_fingerprint(&second).expect("second")
        );
    }

    #[test]
    fn provider_link_addresses_are_not_taken_from_tenant_prefix() {
        let (realm, gateway) = provider_link_addresses(Uuid::from_u128(1), Uuid::from_u128(2));
        assert!(realm.to_string().starts_with("169.254."));
        assert!(gateway.to_string().starts_with("169.254."));
        assert_ne!(realm, gateway);
    }

    #[test]
    fn provider_link_names_bind_gateway_and_attachment_identity() {
        let attachment = plan(Uuid::from_u128(1), Uuid::from_u128(2)).attachments[0].clone();
        let first = link_names(&plan(Uuid::from_u128(1), Uuid::from_u128(2)), &attachment);
        let second = link_names(&plan(Uuid::from_u128(3), Uuid::from_u128(2)), &attachment);
        assert_ne!(first, second);
        assert!(first.0.len() <= 15 && first.1.len() <= 15);
    }

    #[test]
    fn linux_provider_reopens_exact_gateway_snapshot() {
        let root = std::env::temp_dir().join(format!("o3k-gateway-{}", Uuid::now_v7()));
        let realm_id = Uuid::from_u128(3);
        let context = RealmExecutionContext {
            realm_id,
            realm_generation: 1,
            namespace: "o3k-r-00000000".to_owned(),
            bridge: "o3k-b-00000000".to_owned(),
            realm_interface: "o3k-n-00000000".to_owned(),
        };
        let mut contexts = BTreeMap::new();
        contexts.insert(realm_id, context);
        let desired = plan(Uuid::from_u128(1), Uuid::from_u128(11));
        let mut first = LinuxL3GatewayProvider::with_command(
            &root,
            contexts.clone(),
            Arc::new(FakeGatewayCommand::new(false, false)),
        )
        .expect("first provider");
        L3GatewayBackend::apply(&mut first, &desired).expect("apply");
        drop(first);
        let second = LinuxL3GatewayProvider::with_command(
            &root,
            contexts,
            Arc::new(FakeGatewayCommand::new(true, true)),
        )
        .expect("fresh provider");
        assert_eq!(
            second
                .observe(desired.gateway_id, "project-a")
                .expect("observe"),
            Some(desired)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn external_realm_is_realized_as_a_provider_attachment_with_snat() {
        let root = std::env::temp_dir().join(format!("o3k-gateway-external-{}", Uuid::now_v7()));
        let internal_id = Uuid::from_u128(3);
        let external_id = Uuid::from_u128(4);
        let mut contexts = BTreeMap::new();
        for (realm_id, generation) in [(internal_id, 2), (external_id, 3)] {
            contexts.insert(
                realm_id,
                RealmExecutionContext {
                    realm_id,
                    realm_generation: generation,
                    namespace: format!("o3k-r-{}", realm_id.simple()),
                    bridge: format!("o3k-b-{}", realm_id.simple()),
                    realm_interface: format!("o3k-i-{}", &realm_id.simple().to_string()[..8]),
                },
            );
        }
        let mut desired = plan(Uuid::from_u128(1), Uuid::from_u128(11));
        desired.attachments[0].realm_generation = 2;
        desired.external_realm_id = Some(external_id);
        desired.external_realm_prefix =
            Some(Ipv4Prefix::new(Ipv4Addr::new(192, 0, 2, 0), 24).expect("prefix"));
        desired.external_realm_generation = Some(3);
        desired.enable_snat = true;
        let command = Arc::new(FakeGatewayCommand::new(false, false));
        let calls = Arc::clone(&command);
        let mut provider =
            LinuxL3GatewayProvider::with_command(&root, contexts, command).expect("provider");
        provider.apply(&desired).expect("apply");
        assert!(
            calls
                .calls
                .lock()
                .expect("calls")
                .iter()
                .any(|(_, args)| { args.iter().any(|arg| arg == "masquerade") })
        );
        assert!(
            calls
                .calls
                .lock()
                .expect("calls")
                .iter()
                .any(|(_, args)| { args.iter().any(|arg| arg == "192.0.2.0/24") })
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires CAP_NET_ADMIN; run in the privileged Linux provider gate"]
    fn linux_provider_preserves_unaffected_realms_after_fresh_reopen() {
        fn ip(args: &[&str]) -> bool {
            Command::new("ip")
                .args(args)
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        }
        let suffix = Uuid::now_v7().simple().to_string();
        let gateway_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, suffix.as_bytes());
        let gateway_namespace = format!("o3k-gw-{}", &gateway_id.simple().to_string()[..8]);
        let realm_a_ns = format!("o3k-ga-{}", &suffix[..5]);
        let realm_b_ns = format!("o3k-gb-{}", &suffix[..5]);
        let bridge_a = format!("o3k-ba-{}", &suffix[..5]);
        let bridge_b = format!("o3k-bb-{}", &suffix[..5]);
        let endpoint_a_ns = format!("o3k-ea-{}", &suffix[..5]);
        let endpoint_b_ns = format!("o3k-eb-{}", &suffix[..5]);
        let endpoint_a = format!("o3k-ea-i-{}", &suffix[..4]);
        let endpoint_b = format!("o3k-eb-i-{}", &suffix[..4]);
        let endpoint_a_host = format!("o3k-ea-h-{}", &suffix[..4]);
        let endpoint_b_host = format!("o3k-eb-h-{}", &suffix[..4]);
        let old_link_a = "o3kr0000300";
        let old_link_b = "o3kr0000400";
        let root = std::env::temp_dir().join(format!("o3k-gateway-real-{}", &suffix[..8]));
        let cleanup = || {
            let _ = ip(&["netns", "del", &realm_a_ns]);
            let _ = ip(&["netns", "del", &realm_b_ns]);
            let _ = ip(&["netns", "del", &endpoint_a_ns]);
            let _ = ip(&["netns", "del", &endpoint_b_ns]);
            let _ = ip(&["netns", "del", &gateway_namespace]);
            let _ = ip(&["link", "del", &bridge_a]);
            let _ = ip(&["link", "del", &bridge_b]);
            let _ = ip(&["link", "del", old_link_a]);
            let _ = ip(&["link", "del", old_link_b]);
            let _ = ip(&["link", "del", &endpoint_a_host]);
            let _ = ip(&["link", "del", &endpoint_b_host]);
            let _ = fs::remove_dir_all(&root);
        };
        cleanup();
        assert!(ip(&["netns", "add", &realm_a_ns]));
        assert!(ip(&["netns", "add", &realm_b_ns]));
        assert!(ip(&["netns", "add", &endpoint_a_ns]));
        assert!(ip(&["netns", "add", &endpoint_b_ns]));
        assert!(ip(&[
            "netns",
            "exec",
            &realm_a_ns,
            "sysctl",
            "-w",
            "net.ipv4.ip_forward=1",
        ]));
        assert!(ip(&[
            "netns",
            "exec",
            &realm_b_ns,
            "sysctl",
            "-w",
            "net.ipv4.ip_forward=1",
        ]));
        assert!(ip(&["link", "add", &bridge_a, "type", "bridge"]));
        assert!(ip(&["link", "add", &bridge_b, "type", "bridge"]));
        assert!(ip(&["link", "set", &bridge_a, "up"]));
        assert!(ip(&["link", "set", &bridge_b, "up"]));
        let mut contexts = BTreeMap::new();
        for (realm_ns, bridge, realm_id, interface) in [
            (&realm_a_ns, &bridge_a, Uuid::from_u128(3), "o3k-ra-test"),
            (&realm_b_ns, &bridge_b, Uuid::from_u128(4), "o3k-rb-test"),
        ] {
            let realm_text = realm_id.simple().to_string();
            let host_interface = format!("o3k-h{}", &realm_text[25..]);
            assert!(ip(&[
                "link",
                "add",
                interface,
                "type",
                "veth",
                "peer",
                "name",
                &host_interface,
            ]));
            assert!(ip(&["link", "set", interface, "netns", realm_ns]));
            assert!(ip(&["link", "set", &host_interface, "master", bridge]));
            assert!(ip(&["link", "set", &host_interface, "up"]));
            assert!(ip(&[
                "netns", "exec", realm_ns, "ip", "link", "set", interface, "up",
            ]));
            contexts.insert(
                realm_id,
                RealmExecutionContext {
                    realm_id,
                    realm_generation: 1,
                    namespace: realm_ns.clone(),
                    bridge: bridge.clone(),
                    realm_interface: interface.to_owned(),
                },
            );
        }
        for (endpoint_ns, endpoint_if, host_if, bridge, address) in [
            (
                &endpoint_a_ns,
                &endpoint_a,
                &endpoint_a_host,
                &bridge_a,
                "10.30.0.10/24",
            ),
            (
                &endpoint_b_ns,
                &endpoint_b,
                &endpoint_b_host,
                &bridge_b,
                "10.40.0.10/24",
            ),
        ] {
            assert!(ip(&[
                "link",
                "add",
                endpoint_if,
                "type",
                "veth",
                "peer",
                "name",
                host_if,
            ]));
            assert!(ip(&["link", "set", endpoint_if, "netns", endpoint_ns]));
            assert!(ip(&["link", "set", host_if, "master", bridge]));
            assert!(ip(&["link", "set", host_if, "up"]));
            assert!(ip(&[
                "netns",
                "exec",
                endpoint_ns,
                "ip",
                "addr",
                "add",
                address,
                "dev",
                endpoint_if,
            ]));
            assert!(ip(&[
                "netns",
                "exec",
                endpoint_ns,
                "ip",
                "link",
                "set",
                endpoint_if,
                "up",
            ]));
        }
        let attachment_a = L3GatewayExecutionAttachment {
            attachment_id: Uuid::from_u128(11),
            attachment_generation: 1,
            realm_id: Uuid::from_u128(3),
            realm_generation: 1,
            realm_prefix: Ipv4Prefix::new(Ipv4Addr::new(10, 30, 0, 0), 24).expect("prefix"),
            gateway_address: Ipv4Addr::new(10, 30, 0, 1),
        };
        let attachment_b = L3GatewayExecutionAttachment {
            attachment_id: Uuid::from_u128(12),
            attachment_generation: 1,
            realm_id: Uuid::from_u128(4),
            realm_generation: 1,
            realm_prefix: Ipv4Prefix::new(Ipv4Addr::new(10, 40, 0, 0), 24).expect("prefix"),
            gateway_address: Ipv4Addr::new(10, 40, 0, 1),
        };
        let mut initial = plan(gateway_id, attachment_a.attachment_id);
        initial.attachments = vec![attachment_a.clone(), attachment_b.clone()];
        let mut updated = initial.clone();
        updated.attachments[0].attachment_generation = 2;
        let mut provider = LinuxL3GatewayProvider::open(&root, contexts.clone()).expect("open");
        provider.apply(&initial).expect("initial apply");
        assert!(ip(&[
            "netns",
            "exec",
            &endpoint_a_ns,
            "ip",
            "route",
            "replace",
            "10.40.0.0/24",
            "via",
            "10.30.0.1",
        ]));
        assert!(ip(&[
            "netns",
            "exec",
            &endpoint_b_ns,
            "ip",
            "route",
            "replace",
            "10.30.0.0/24",
            "via",
            "10.40.0.1",
        ]));
        let listener = Command::new("ip")
            .args([
                "netns",
                "exec",
                &endpoint_b_ns,
                "python3",
                "-c",
                "import socket; s=socket.socket(); s.bind(('10.40.0.10',18080)); s.listen(1); c,_=s.accept(); c.close()",
            ])
            .spawn()
            .expect("listener");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let connection = Command::new("ip")
            .args([
                "netns",
                "exec",
                &endpoint_a_ns,
                "python3",
                "-c",
                "import socket; s=socket.create_connection(('10.40.0.10',18080), 2); s.close()",
            ])
            .status()
            .expect("connection");
        assert!(connection.success());
        let _ = listener.wait_with_output();
        drop(provider);
        let mut provider = LinuxL3GatewayProvider::open(&root, contexts.clone()).expect("reopen");
        assert_eq!(
            provider
                .observe(initial.gateway_id, "project-a")
                .expect("observe"),
            Some(initial.clone())
        );
        provider.apply(&updated).expect("update A");
        drop(provider);
        let mut provider = LinuxL3GatewayProvider::open(&root, contexts.clone()).expect("reopen 2");
        assert_eq!(
            provider
                .observe(updated.gateway_id, "project-a")
                .expect("observe 2"),
            Some(updated.clone())
        );
        let mut only_b = updated.clone();
        only_b.attachments = vec![attachment_b];
        provider.apply(&only_b).expect("remove A");
        let listener = Command::new("ip")
            .args([
                "netns",
                "exec",
                &endpoint_b_ns,
                "python3",
                "-c",
                "import socket; s=socket.socket(); s.bind(('10.40.0.10',18080)); s.listen(1); s.settimeout(2); c,_=s.accept(); c.close()",
            ])
            .spawn()
            .expect("listener after detach");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let blocked = Command::new("ip")
            .args([
                "netns",
                "exec",
                &endpoint_a_ns,
                "python3",
                "-c",
                "import socket; socket.create_connection(('10.40.0.10',18080), 1)",
            ])
            .status()
            .expect("blocked connection");
        assert!(!blocked.success());
        let _ = listener.wait_with_output();
        drop(provider);
        let provider = LinuxL3GatewayProvider::open(&root, contexts).expect("reopen 3");
        assert_eq!(
            provider
                .observe(only_b.gateway_id, "project-a")
                .expect("observe 3"),
            Some(only_b)
        );
        cleanup();
    }
}
