//! Provider-independent execution boundary for canonical multi-Realm gateways.
//!
//! `NamespacedRoutedFabricPlan` remains a one-Realm fabric plan.  This module
//! owns the separate execution unit for an L3 gateway and deliberately keeps
//! provider names (Linux namespaces, links, and routing tables) out of the
//! semantic plan.

use o3k_domain::{Ipv4Prefix, L3GatewayExecutionAttachment, L3GatewayExecutionPlan};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, net::Ipv4Addr};
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

pub use crate::linux_fabric::gateway::LinuxL3GatewayProvider;

pub fn gateway_plan_fingerprint(plan: &L3GatewayExecutionPlan) -> Result<String, L3GatewayError> {
    let mut canonical = plan.clone();
    canonical
        .attachments
        .sort_by_key(|attachment| attachment.attachment_id);
    validate_plan(&canonical)?;
    let bytes = serde_json::to_vec(&canonical).map_err(|_| L3GatewayError::Serialization)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn parse_prefix(value: &str) -> Result<Ipv4Prefix, L3GatewayError> {
    let (network, prefix_len) = value.split_once('/').ok_or(L3GatewayError::InvalidPlan)?;
    let network = network.parse().map_err(|_| L3GatewayError::InvalidPlan)?;
    let prefix_len = prefix_len
        .parse()
        .map_err(|_| L3GatewayError::InvalidPlan)?;
    Ipv4Prefix::new(network, prefix_len).ok_or(L3GatewayError::InvalidPlan)
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
