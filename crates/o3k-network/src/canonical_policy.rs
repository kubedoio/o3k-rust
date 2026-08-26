//! Compilation of reusable canonical policy state into endpoint execution
//! intents. This module deliberately has no OpenStack or provider types.

use o3k_domain::{
    NetworkPlanIntent, NetworkProtocol, PolicyAction, PolicyDefaultIntent, PolicyDirection,
    PolicyIntent, PolicyStatefulMode, PortRange,
};
use o3k_store::{
    CanonicalAddressRealmRecord, CanonicalEndpointRecord, CanonicalNetworkPolicyRuleRecord,
    CanonicalPolicyAttachmentRecord, CanonicalReusableNetworkPolicyRecord,
};
use thiserror::Error;
#[cfg(test)]
use uuid::Uuid;

use crate::{NetworkPlanError, NodeNetworkPlan, canonical_plan_fingerprint};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CanonicalPolicyCompileError {
    #[error("canonical policy graph has invalid ownership")]
    Ownership,
    #[error("canonical policy graph has an invalid lifecycle or generation")]
    InvalidState,
    #[error("canonical policy graph has incompatible unmatched actions")]
    ConflictingDefaults,
    #[error("canonical policy graph has unsupported stateful mode")]
    UnsupportedStatefulMode,
    #[error("canonical policy graph has invalid rule semantics")]
    InvalidRule,
    #[error("canonical policy graph does not match the endpoint realm")]
    RealmMismatch,
}

/// Compile all active policies attached to one endpoint. Empty output is
/// intentional: an endpoint without a policy retains the existing baseline.
pub fn compile_endpoint_policy(
    endpoint: &CanonicalEndpointRecord,
    realm: &CanonicalAddressRealmRecord,
    policies: &[CanonicalReusableNetworkPolicyRecord],
    rules: &[CanonicalNetworkPolicyRuleRecord],
    attachments: &[CanonicalPolicyAttachmentRecord],
) -> Result<Vec<NetworkPlanIntent>, CanonicalPolicyCompileError> {
    if endpoint.id.is_nil()
        || endpoint.project_id.is_empty()
        || realm.id != endpoint.realm_id
        || realm.project_id != endpoint.project_id
    {
        return Err(CanonicalPolicyCompileError::RealmMismatch);
    }
    if realm.state != "active" {
        return Err(CanonicalPolicyCompileError::InvalidState);
    }
    let realm_prefix =
        parse_prefix(Some(&realm.prefix))?.ok_or(CanonicalPolicyCompileError::RealmMismatch)?;
    if !realm_prefix.contains(endpoint.fixed_ip) {
        return Err(CanonicalPolicyCompileError::RealmMismatch);
    }

    let mut active_attachments = attachments
        .iter()
        .filter(|attachment| attachment.endpoint_id == endpoint.id && attachment.state == "active")
        .collect::<Vec<_>>();
    active_attachments.sort_by_key(|attachment| attachment.id);

    let mut selected = Vec::new();
    for attachment in active_attachments {
        if attachment.project_id != endpoint.project_id || attachment.policy_id.is_nil() {
            return Err(CanonicalPolicyCompileError::Ownership);
        }
        let policy = policies
            .iter()
            .find(|policy| policy.id == attachment.policy_id)
            .ok_or(CanonicalPolicyCompileError::Ownership)?;
        if policy.project_id != endpoint.project_id
            || policy.state != "active"
            || policy.generation == 0
            || attachment.generation == 0
        {
            return Err(CanonicalPolicyCompileError::InvalidState);
        }
        if policy.stateful_mode != "Stateful" {
            return Err(CanonicalPolicyCompileError::UnsupportedStatefulMode);
        }
        let action = parse_action(&policy.unmatched_action)?;
        if selected
            .first()
            .is_some_and(|(_, existing)| *existing != action)
        {
            return Err(CanonicalPolicyCompileError::ConflictingDefaults);
        }
        selected.push((policy, action));
    }
    if selected.is_empty() {
        return Ok(Vec::new());
    }

    let (_, unmatched_action) = selected[0];
    let generation = selected
        .iter()
        .map(|(policy, _)| policy.generation)
        .max()
        .ok_or(CanonicalPolicyCompileError::InvalidState)?;
    let mut output = vec![NetworkPlanIntent::PolicyDefault(PolicyDefaultIntent {
        policy_id: selected[0].0.id,
        endpoint_id: endpoint.id,
        unmatched_action,
        stateful_mode: PolicyStatefulMode::Stateful,
        generation,
    })];

    let policy_ids = selected
        .iter()
        .map(|(policy, _)| policy.id)
        .collect::<Vec<_>>();
    for rule in rules
        .iter()
        .filter(|rule| policy_ids.contains(&rule.policy_id))
    {
        if rule.project_id != endpoint.project_id {
            return Err(CanonicalPolicyCompileError::Ownership);
        }
    }
    let mut selected_rules = rules
        .iter()
        .filter(|rule| rule.state == "active" && policy_ids.contains(&rule.policy_id))
        .collect::<Vec<_>>();
    selected_rules.sort_by_key(|rule| rule.id);
    for rule in selected_rules {
        if rule.generation == 0
            || rule.id.is_nil()
            || rule.policy_id.is_nil()
            || rule.address_family != "Ipv4"
            || rule.port_min.is_some() != rule.port_max.is_some()
        {
            return Err(CanonicalPolicyCompileError::InvalidState);
        }
        let direction = parse_direction(&rule.direction)?;
        let protocol = parse_protocol(&rule.protocol)?;
        let source = if direction == PolicyDirection::Ingress {
            parse_prefix(rule.remote_selector.as_deref())?
        } else {
            None
        };
        let destination = if direction == PolicyDirection::Egress {
            parse_prefix(rule.remote_selector.as_deref())?
        } else {
            None
        };
        output.push(NetworkPlanIntent::Policy(PolicyIntent {
            id: rule.id,
            endpoint_id: endpoint.id,
            direction,
            protocol,
            ports: rule
                .port_min
                .zip(rule.port_max)
                .map(|(start, end)| PortRange { start, end }),
            source,
            destination,
            action: parse_action(&rule.action)?,
        }));
    }
    Ok(output)
}

/// Replace the derived policy portion of an existing node plan with one
/// complete canonical Endpoint snapshot. Existing non-policy network intents
/// are preserved and the plan fingerprint is rebound to the canonical
/// generations involved in the snapshot.
pub fn attach_endpoint_policy_to_plan(
    mut plan: NodeNetworkPlan,
    endpoint: &CanonicalEndpointRecord,
    realm: &CanonicalAddressRealmRecord,
    policies: &[CanonicalReusableNetworkPolicyRecord],
    rules: &[CanonicalNetworkPolicyRuleRecord],
    attachments: &[CanonicalPolicyAttachmentRecord],
) -> Result<NodeNetworkPlan, NetworkPlanError> {
    let compiled = compile_endpoint_policy(endpoint, realm, policies, rules, attachments)
        .map_err(|_| NetworkPlanError::InvalidPolicy)?;
    plan.intents.retain(|intent| {
        !matches!(
            intent,
            NetworkPlanIntent::Policy(policy) if policy.endpoint_id == endpoint.id
        ) && !matches!(
            intent,
            NetworkPlanIntent::PolicyDefault(default) if default.endpoint_id == endpoint.id
        )
    });
    plan.intents.extend(compiled.clone());
    plan.intents
        .sort_by_key(|intent| serde_json::to_string(intent).unwrap_or_default());
    for policy in policies {
        plan.resource_generations
            .insert(policy.id, policy.generation);
    }
    for rule in rules {
        plan.resource_generations.insert(rule.id, rule.generation);
    }
    for attachment in attachments {
        plan.resource_generations
            .insert(attachment.id, attachment.generation);
    }
    if let Some(mut fabric) = plan.fabric.take() {
        let current_generation = fabric.policy_generation;
        fabric
            .policies
            .retain(|policy| policy.endpoint_id != endpoint.id);
        fabric
            .policy_defaults
            .retain(|default| default.endpoint_id != endpoint.id);
        let mut fabric_policies = fabric.policies.clone();
        let mut fabric_defaults = fabric.policy_defaults.clone();
        for intent in &compiled {
            match intent {
                NetworkPlanIntent::Policy(policy) => fabric_policies.push(policy.clone()),
                NetworkPlanIntent::PolicyDefault(default) => fabric_defaults.push(default.clone()),
                _ => {}
            }
        }
        let compiled_generation = policies
            .iter()
            .map(|policy| policy.generation)
            .chain(rules.iter().map(|rule| rule.generation))
            .chain(attachments.iter().map(|attachment| attachment.generation))
            .max()
            .unwrap_or(current_generation);
        fabric = fabric
            .with_canonical_policy_snapshot(
                current_generation.max(compiled_generation),
                fabric_defaults,
                fabric_policies,
            )
            .map_err(|_| NetworkPlanError::InvalidFabricPlan)?;
        plan.fabric = Some(fabric);
    }
    plan.fingerprint_sha256 = canonical_plan_fingerprint(&plan)?;
    Ok(plan)
}

fn parse_action(value: &str) -> Result<PolicyAction, CanonicalPolicyCompileError> {
    match value {
        "Allow" | "allow" => Ok(PolicyAction::Allow),
        "Deny" | "deny" => Ok(PolicyAction::Deny),
        _ => Err(CanonicalPolicyCompileError::InvalidState),
    }
}

fn parse_direction(value: &str) -> Result<PolicyDirection, CanonicalPolicyCompileError> {
    match value {
        "Ingress" | "ingress" => Ok(PolicyDirection::Ingress),
        "Egress" | "egress" => Ok(PolicyDirection::Egress),
        _ => Err(CanonicalPolicyCompileError::InvalidRule),
    }
}

fn parse_protocol(value: &str) -> Result<NetworkProtocol, CanonicalPolicyCompileError> {
    match value {
        "Any" | "any" => Ok(NetworkProtocol::Any),
        "Tcp" | "tcp" => Ok(NetworkProtocol::Tcp),
        "Udp" | "udp" => Ok(NetworkProtocol::Udp),
        "Icmp" | "icmp" => Ok(NetworkProtocol::Icmp),
        _ => Err(CanonicalPolicyCompileError::InvalidRule),
    }
}

fn parse_prefix(
    value: Option<&str>,
) -> Result<Option<o3k_domain::Ipv4Prefix>, CanonicalPolicyCompileError> {
    let Some(value) = value else { return Ok(None) };
    let (address, length) = value
        .split_once('/')
        .ok_or(CanonicalPolicyCompileError::InvalidRule)?;
    let address = address
        .parse()
        .map_err(|_| CanonicalPolicyCompileError::InvalidRule)?;
    let length = length
        .parse()
        .map_err(|_| CanonicalPolicyCompileError::InvalidRule)?;
    o3k_domain::Ipv4Prefix::new(address, length)
        .map(Some)
        .ok_or(CanonicalPolicyCompileError::InvalidRule)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn endpoint() -> CanonicalEndpointRecord {
        CanonicalEndpointRecord {
            id: Uuid::from_u128(1),
            realm_id: Uuid::from_u128(2),
            project_id: "p".into(),
            fixed_ip: Ipv4Addr::new(10, 0, 0, 2),
            mac: "02:00:00:00:00:02".into(),
            generation: 1,
            state: "active".into(),
        }
    }
    fn realm() -> CanonicalAddressRealmRecord {
        CanonicalAddressRealmRecord {
            id: Uuid::from_u128(2),
            network_id: Uuid::from_u128(3),
            project_id: "p".into(),
            prefix: "10.0.0.0/24".into(),
            overlapping_prefixes: false,
            generation: 1,
            state: "active".into(),
        }
    }
    fn policy(action: &str) -> CanonicalReusableNetworkPolicyRecord {
        CanonicalReusableNetworkPolicyRecord {
            id: Uuid::from_u128(4),
            project_id: "p".into(),
            name: "p".into(),
            description: String::new(),
            stateful_mode: "Stateful".into(),
            unmatched_action: action.into(),
            generation: 2,
            state: "active".into(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
    fn attachment() -> CanonicalPolicyAttachmentRecord {
        CanonicalPolicyAttachmentRecord {
            id: Uuid::from_u128(5),
            policy_id: Uuid::from_u128(4),
            endpoint_id: Uuid::from_u128(1),
            project_id: "p".into(),
            state: "active".into(),
            generation: 1,
        }
    }
    #[test]
    fn no_policy_preserves_baseline() {
        assert!(
            compile_endpoint_policy(&endpoint(), &realm(), &[], &[], &[])
                .expect("empty policy graph")
                .is_empty()
        );
    }
    #[test]
    fn zero_rule_policy_emits_default() {
        let result = compile_endpoint_policy(
            &endpoint(),
            &realm(),
            &[policy("Deny")],
            &[],
            &[attachment()],
        )
        .expect("deny default compilation");
        assert!(matches!(
            result[0],
            NetworkPlanIntent::PolicyDefault(PolicyDefaultIntent {
                unmatched_action: PolicyAction::Deny,
                ..
            })
        ));
    }
    #[test]
    fn mixed_defaults_are_rejected() {
        let mut second = attachment();
        second.id = Uuid::from_u128(6);
        second.policy_id = Uuid::from_u128(7);
        assert_eq!(
            compile_endpoint_policy(
                &endpoint(),
                &realm(),
                &[
                    policy("Allow"),
                    CanonicalReusableNetworkPolicyRecord {
                        id: Uuid::from_u128(7),
                        ..policy("Deny")
                    }
                ],
                &[],
                &[attachment(), second]
            ),
            Err(CanonicalPolicyCompileError::ConflictingDefaults)
        );
    }

    #[test]
    fn active_rules_are_compiled_with_stable_rule_identity() {
        let rule = CanonicalNetworkPolicyRuleRecord {
            id: Uuid::from_u128(8),
            policy_id: Uuid::from_u128(4),
            project_id: "p".into(),
            direction: "Ingress".into(),
            address_family: "Ipv4".into(),
            protocol: "Tcp".into(),
            port_min: Some(443),
            port_max: Some(443),
            remote_selector: Some("198.51.100.0/24".into()),
            action: "Allow".into(),
            state: "active".into(),
            generation: 3,
            enforcement_key: "canonical".into(),
        };
        let result = compile_endpoint_policy(
            &endpoint(),
            &realm(),
            &[policy("Deny")],
            &[rule],
            &[attachment()],
        )
        .expect("rule compilation");
        assert!(matches!(
            result[1],
            NetworkPlanIntent::Policy(PolicyIntent {
                id,
                action: PolicyAction::Allow,
                ..
            }) if id == Uuid::from_u128(8)
        ));
    }

    #[test]
    fn compatible_policies_compile_as_one_endpoint_snapshot() {
        let mut second_policy = policy("Deny");
        second_policy.id = Uuid::from_u128(7);
        let mut second_attachment = attachment();
        second_attachment.id = Uuid::from_u128(6);
        second_attachment.policy_id = second_policy.id;
        let first_rule = CanonicalNetworkPolicyRuleRecord {
            id: Uuid::from_u128(8),
            policy_id: Uuid::from_u128(4),
            project_id: "p".into(),
            direction: "Ingress".into(),
            address_family: "Ipv4".into(),
            protocol: "Tcp".into(),
            port_min: Some(80),
            port_max: Some(80),
            remote_selector: Some("198.51.100.0/24".into()),
            action: "Allow".into(),
            state: "active".into(),
            generation: 1,
            enforcement_key: "first".into(),
        };
        let mut second_rule = first_rule.clone();
        second_rule.id = Uuid::from_u128(9);
        second_rule.policy_id = second_policy.id;
        second_rule.port_min = Some(443);
        second_rule.port_max = Some(443);
        let result = compile_endpoint_policy(
            &endpoint(),
            &realm(),
            &[policy("Deny"), second_policy],
            &[first_rule, second_rule],
            &[attachment(), second_attachment],
        )
        .expect("compatible policy composition");
        assert_eq!(result.len(), 3);
        assert!(matches!(
            result[0],
            NetworkPlanIntent::PolicyDefault(PolicyDefaultIntent {
                unmatched_action: PolicyAction::Deny,
                ..
            })
        ));
        assert!(matches!(
            result[1],
            NetworkPlanIntent::Policy(PolicyIntent { id, .. }) if id == Uuid::from_u128(8)
        ));
        assert!(matches!(
            result[2],
            NetworkPlanIntent::Policy(PolicyIntent { id, .. }) if id == Uuid::from_u128(9)
        ));
    }
}
