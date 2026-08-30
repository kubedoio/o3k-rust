use super::helpers::{
    parse_security_group_direction, parse_security_group_prefix, parse_security_group_protocol,
};
use super::{NetworkError, NetworkService, map_store_error};
use crate::NetworkRecord;
use crate::plan::{
    canonical_policy_record, policy_from_canonical_record, security_group_from_policy,
    security_group_rule_from_policy, validate_policy_shape,
};
use o3k_domain::{
    NetworkProtocol, PolicyAction, PolicyDefaultIntent, PolicyDirection, PolicyIntent,
    PolicyStatefulMode, PortRange,
};
use o3k_kernel::{
    ActionId, AuditEvent, AuditOutcome, AuthContext, AuthorizationRequest, LimitKey,
    OwnershipScope, ResourceAmount, ResourceId, ResourceTarget, ResourceType, ScopeId,
    ServiceNamespace,
};
use uuid::Uuid;

fn canonical_network_projection(network: o3k_store::CanonicalNetworkRecord) -> NetworkRecord {
    NetworkRecord {
        id: network.id,
        name: network.name,
        project_id: network.project_id,
        status: network.state.to_ascii_uppercase(),
    }
}

impl NetworkService {
    pub async fn create_network(
        &self,
        auth: &AuthContext,
        name: String,
    ) -> Result<NetworkRecord, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "CreateNetwork").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "CreateNetwork".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("network", "network")
                    .map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::Unauthorized);
        }
        match self
            .create_network_for_project(auth.effective_scope().id().as_str(), name)
            .await
        {
            Ok(record) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("network", "network").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("network".to_owned(), "network".to_owned())
                        }),
                        ResourceId::new(record.id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(record)
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn create_network_for_project(
        &self,
        project_id: &str,
        name: String,
    ) -> Result<NetworkRecord, NetworkError> {
        if name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        if self
            .inner
            .repository
            .list_canonical_networks(project_id)
            .await
            .map_err(map_store_error)?
            .iter()
            .any(|network| network.name == name)
        {
            return Err(NetworkError::Conflict);
        }
        let network = NetworkRecord {
            id: Uuid::now_v7(),
            name,
            project_id: project_id.to_owned(),
            status: "ACTIVE".to_owned(),
        };
        let canonical = o3k_store::CanonicalNetworkRecord {
            id: network.id,
            project_id: network.project_id.clone(),
            name: network.name.clone(),
            admin_state_up: true,
            generation: 1,
            state: "active".to_owned(),
        };
        let scope =
            OwnershipScope::project(ScopeId::new_unchecked(project_id.to_owned()), None, None);
        let amounts = vec![ResourceAmount::new(LimitKey::network_networks(), 1)];
        let op_id = format!("o3k:network:create:{}:{}", project_id, network.id);
        let quota_res = self
            .inner
            .repository
            .reserve_quota(&scope, &op_id, &amounts)
            .await
            .map_err(|err| match err {
                o3k_store::StoreError::QuotaExceeded {
                    key,
                    limit,
                    used,
                    requested,
                } => NetworkError::QuotaExceeded {
                    key,
                    limit,
                    used,
                    requested,
                },
                o3k_store::StoreError::ReservationConflict(_) => NetworkError::Conflict,
                other => map_store_error(other),
            })?;

        match self
            .inner
            .repository
            .insert_canonical_network(&canonical)
            .await
        {
            Ok(()) => {
                if let Err(error) = self.inner.repository.insert_network(&network).await {
                    let _ = self
                        .inner
                        .repository
                        .delete_canonical_network(project_id, &network.id)
                        .await;
                    let _ = self
                        .inner
                        .repository
                        .release_reservation(&quota_res.id)
                        .await;
                    return Err(map_store_error(error));
                }
                let _ = self
                    .inner
                    .repository
                    .commit_reservation(&quota_res.id)
                    .await;
                Ok(network)
            }
            Err(o3k_store::StoreError::ResourceAlreadyExists) => {
                let _ = self
                    .inner
                    .repository
                    .release_reservation(&quota_res.id)
                    .await;
                Err(NetworkError::Conflict)
            }
            Err(error) => {
                let _ = self
                    .inner
                    .repository
                    .release_reservation(&quota_res.id)
                    .await;
                Err(map_store_error(error))
            }
        }
    }

    pub async fn list_security_groups_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<o3k_store::SecurityGroupRecord>, NetworkError> {
        self.inner
            .repository
            .list_reusable_policies(project_id)
            .await
            .map(|policies| {
                policies
                    .into_iter()
                    .map(security_group_from_policy)
                    .collect()
            })
            .map_err(map_store_error)
    }

    pub async fn get_security_group_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<o3k_store::SecurityGroupRecord, NetworkError> {
        self.inner
            .repository
            .get_reusable_policy(project_id, &id)
            .await
            .map_err(map_store_error)?
            .map(security_group_from_policy)
            .ok_or(NetworkError::NotFound)
    }

    pub async fn create_security_group_for_project(
        &self,
        project_id: &str,
        name: String,
        description: String,
    ) -> Result<o3k_store::SecurityGroupRecord, NetworkError> {
        if project_id.trim().is_empty() || name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        let group = o3k_store::SecurityGroupRecord {
            id: Uuid::now_v7(),
            project_id: project_id.to_owned(),
            name,
            description,
        };
        self.inner
            .repository
            .insert_reusable_policy(&o3k_store::CanonicalReusableNetworkPolicyRecord {
                id: group.id,
                project_id: group.project_id.clone(),
                name: group.name.clone(),
                description: group.description.clone(),
                stateful_mode: "Stateful".to_owned(),
                unmatched_action: "Deny".to_owned(),
                generation: 1,
                state: "active".to_owned(),
                created_at: "2026-08-26T00:00:00Z".to_owned(),
                updated_at: "2026-08-26T00:00:00Z".to_owned(),
            })
            .await
            .map_err(map_store_error)?;
        Ok(group)
    }

    pub async fn update_security_group_for_project(
        &self,
        project_id: &str,
        id: Uuid,
        name: String,
        description: String,
    ) -> Result<o3k_store::SecurityGroupRecord, NetworkError> {
        if name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        let current = self
            .inner
            .repository
            .get_reusable_policy(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        let updated = self
            .inner
            .repository
            .update_reusable_policy(
                &o3k_store::CanonicalReusableNetworkPolicyRecord {
                    name,
                    description,
                    updated_at: "2026-08-26T00:00:00Z".to_owned(),
                    generation: current.generation.saturating_add(1),
                    ..current
                },
                current.generation,
            )
            .await
            .map_err(map_store_error)?;
        Ok(security_group_from_policy(updated))
    }

    pub async fn delete_security_group_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .delete_reusable_policy(project_id, &id)
            .await
            .map_err(map_store_error)
    }

    pub async fn list_security_group_rules_for_project(
        &self,
        project_id: &str,
        group_id: Uuid,
    ) -> Result<Vec<o3k_store::SecurityGroupRuleRecord>, NetworkError> {
        if self
            .inner
            .repository
            .get_reusable_policy(project_id, &group_id)
            .await
            .map_err(map_store_error)?
            .is_none()
        {
            return Err(NetworkError::NotFound);
        }
        self.inner
            .repository
            .list_policy_rules(project_id, &group_id)
            .await
            .map(|rules| {
                rules
                    .into_iter()
                    .map(security_group_rule_from_policy)
                    .collect()
            })
            .map_err(map_store_error)
    }

    pub async fn get_security_group_rule_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<o3k_store::SecurityGroupRuleRecord, NetworkError> {
        self.inner
            .repository
            .get_policy_rule(project_id, &id)
            .await
            .map_err(map_store_error)?
            .map(security_group_rule_from_policy)
            .ok_or(NetworkError::NotFound)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_security_group_rule_for_project(
        &self,
        project_id: &str,
        group_id: Uuid,
        direction: String,
        protocol: String,
        port_min: Option<u16>,
        port_max: Option<u16>,
        remote_ip_prefix: Option<String>,
    ) -> Result<o3k_store::SecurityGroupRuleRecord, NetworkError> {
        let direction = parse_security_group_direction(&direction)?;
        let protocol_value = parse_security_group_protocol(&protocol)?;
        if matches!(protocol_value, NetworkProtocol::Icmp | NetworkProtocol::Any)
            && (port_min.is_some() || port_max.is_some())
        {
            return Err(NetworkError::InvalidRequest);
        }
        match (port_min, port_max) {
            (Some(start), Some(end)) if start <= end => {}
            (None, None) => {}
            _ => return Err(NetworkError::InvalidRequest),
        }
        if let Some(prefix) = remote_ip_prefix.as_deref() {
            parse_security_group_prefix(prefix)?;
        }
        let _guard = self.lock().await;
        if self
            .inner
            .repository
            .get_reusable_policy(project_id, &group_id)
            .await
            .map_err(map_store_error)?
            .is_none()
        {
            return Err(NetworkError::NotFound);
        }
        let rule = o3k_store::CanonicalNetworkPolicyRuleRecord {
            id: Uuid::now_v7(),
            policy_id: group_id,
            project_id: project_id.to_owned(),
            direction: match direction {
                PolicyDirection::Ingress => "Ingress",
                PolicyDirection::Egress => "Egress",
            }
            .to_owned(),
            protocol: match protocol_value {
                NetworkProtocol::Any => "Any",
                NetworkProtocol::Tcp => "Tcp",
                NetworkProtocol::Udp => "Udp",
                NetworkProtocol::Icmp => "Icmp",
            }
            .to_owned(),
            address_family: "Ipv4".to_owned(),
            port_min,
            port_max,
            remote_selector: remote_ip_prefix,
            action: "Allow".to_owned(),
            state: "active".to_owned(),
            generation: 1,
            enforcement_key: String::new(),
        };
        let remote = rule
            .remote_selector
            .clone()
            .unwrap_or_else(|| "-".to_owned());
        let ports = rule
            .port_min
            .zip(rule.port_max)
            .map_or_else(|| "-".to_owned(), |(min, max)| format!("{min}-{max}"));
        let mut rule = rule;
        rule.enforcement_key = format!(
            "{}|{}|{}|{}|{}|{}",
            rule.direction, rule.address_family, rule.protocol, ports, remote, rule.action
        );
        self.inner
            .repository
            .insert_policy_rule(&rule)
            .await
            .map_err(map_store_error)?;
        Ok(security_group_rule_from_policy(rule))
    }

    pub async fn delete_security_group_rule_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .delete_policy_rule(project_id, &id)
            .await
            .map_err(map_store_error)
    }

    pub async fn begin_security_group_rule_deletion_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<o3k_store::CanonicalNetworkPolicyRuleRecord, NetworkError> {
        let _guard = self.lock().await;
        let rule = self
            .inner
            .repository
            .get_policy_rule(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        self.inner
            .repository
            .begin_policy_rule_deletion(project_id, &id, rule.generation)
            .await
            .map_err(map_store_error)
    }

    pub async fn finalize_security_group_rule_deletion_for_project(
        &self,
        project_id: &str,
        id: Uuid,
        deleting_generation: u64,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .finalize_policy_rule_deletion(project_id, &id, deleting_generation)
            .await
            .map_err(map_store_error)
    }

    pub async fn list_security_group_bindings_for_project(
        &self,
        project_id: &str,
        endpoint_id: Option<Uuid>,
    ) -> Result<Vec<o3k_store::SecurityGroupBindingRecord>, NetworkError> {
        let attachments = if let Some(endpoint_id) = endpoint_id {
            self.inner
                .repository
                .list_endpoint_policy_attachments(project_id, &endpoint_id)
                .await
                .map_err(map_store_error)?
        } else {
            let policies = self
                .inner
                .repository
                .list_reusable_policies(project_id)
                .await
                .map_err(map_store_error)?;
            let mut all = Vec::new();
            for policy in policies {
                all.extend(
                    self.inner
                        .repository
                        .list_policy_attachments(project_id, &policy.id)
                        .await
                        .map_err(map_store_error)?,
                );
            }
            all
        };
        Ok(attachments
            .into_iter()
            .filter(|attachment| attachment.state == "active")
            .map(|attachment| o3k_store::SecurityGroupBindingRecord {
                project_id: attachment.project_id,
                endpoint_id: attachment.endpoint_id,
                security_group_id: attachment.policy_id,
            })
            .collect())
    }

    pub async fn replace_security_group_bindings_for_project(
        &self,
        project_id: &str,
        endpoint_id: Uuid,
        group_ids: Vec<Uuid>,
    ) -> Result<Vec<o3k_store::CanonicalPolicyAttachmentRecord>, NetworkError> {
        let _guard = self.lock().await;
        if self
            .inner
            .repository
            .get_port(project_id, &endpoint_id)
            .await
            .map_err(map_store_error)?
            .is_none()
        {
            return Err(NetworkError::NotFound);
        }
        self.inner
            .repository
            .replace_policy_attachment_set(project_id, &endpoint_id, &group_ids)
            .await
            .map_err(map_store_error)
    }

    pub async fn finalize_policy_attachment_deletion_for_project(
        &self,
        project_id: &str,
        attachment_id: Uuid,
        deleting_generation: u64,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .finalize_policy_attachment_deletion(project_id, &attachment_id, deleting_generation)
            .await
            .map_err(map_store_error)
    }

    /// Returns the durable canonical policy rules for a network. A network
    /// without policy state is intentionally an empty policy, not an implicit
    /// provider default.
    pub async fn list_policies_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
    ) -> Result<Vec<PolicyIntent>, NetworkError> {
        if self
            .inner
            .repository
            .get_canonical_network(project_id, &network_id)
            .await
            .map_err(map_store_error)?
            .is_none()
        {
            return Err(NetworkError::NotFound);
        }
        let mut policies = self
            .inner
            .repository
            .list_canonical_policies(project_id, &network_id)
            .await
            .map_err(map_store_error)?
            .into_iter()
            .map(policy_from_canonical_record)
            .collect::<Result<Vec<_>, _>>()?;
        for port in self
            .list_ports_for_project(project_id)
            .await?
            .into_iter()
            .filter(|port| port.network_id == network_id)
        {
            let bindings = self
                .inner
                .repository
                .list_endpoint_policy_attachments(project_id, &port.id)
                .await
                .map_err(map_store_error)?;
            for binding in bindings
                .into_iter()
                .filter(|binding| binding.state == "active")
            {
                let Some(group) = self
                    .inner
                    .repository
                    .get_reusable_policy(project_id, &binding.policy_id)
                    .await
                    .map_err(map_store_error)?
                else {
                    return Err(NetworkError::InvalidRequest);
                };
                for rule in self
                    .inner
                    .repository
                    .list_policy_rules(project_id, &group.id)
                    .await
                    .map_err(map_store_error)?
                    .into_iter()
                    .filter(|rule| rule.state == "active")
                {
                    let direction = parse_security_group_direction(&rule.direction)?;
                    let remote = rule
                        .remote_selector
                        .as_deref()
                        .map(parse_security_group_prefix)
                        .transpose()?;
                    let ports = match (rule.port_min, rule.port_max) {
                        (Some(start), Some(end)) => Some(PortRange { start, end }),
                        (None, None) => None,
                        _ => return Err(NetworkError::InvalidRequest),
                    };
                    policies.push(PolicyIntent {
                        id: rule.id,
                        endpoint_id: port.id,
                        direction,
                        protocol: parse_security_group_protocol(&rule.protocol)?,
                        ports,
                        source: (direction == PolicyDirection::Ingress)
                            .then_some(remote)
                            .flatten(),
                        destination: (direction == PolicyDirection::Egress)
                            .then_some(remote)
                            .flatten(),
                        action: PolicyAction::Allow,
                    });
                }
            }
        }
        policies.sort_by_key(|policy| policy.id);
        Ok(policies)
    }

    /// Resolves canonical unmatched-action defaults for the active policies
    /// attached to one endpoint. Defaults are derived execution input; the
    /// reusable policy repository remains the sole desired-state authority.
    pub async fn policy_defaults_for_endpoint(
        &self,
        project_id: &str,
        endpoint_id: Uuid,
    ) -> Result<Vec<PolicyDefaultIntent>, NetworkError> {
        let attachments = self
            .inner
            .repository
            .list_endpoint_policy_attachments(project_id, &endpoint_id)
            .await
            .map_err(map_store_error)?;
        let mut defaults = Vec::new();
        for attachment in attachments.into_iter().filter(|a| a.state == "active") {
            let policy = self
                .inner
                .repository
                .get_reusable_policy(project_id, &attachment.policy_id)
                .await
                .map_err(map_store_error)?
                .ok_or(NetworkError::InvalidRequest)?;
            if policy.state != "active" || policy.stateful_mode != "Stateful" {
                return Err(NetworkError::InvalidRequest);
            }
            let unmatched_action = match policy.unmatched_action.as_str() {
                "Allow" => PolicyAction::Allow,
                "Deny" => PolicyAction::Deny,
                _ => return Err(NetworkError::InvalidRequest),
            };
            defaults.push(PolicyDefaultIntent {
                policy_id: policy.id,
                endpoint_id,
                unmatched_action,
                stateful_mode: PolicyStatefulMode::Stateful,
                generation: policy.generation.max(attachment.generation),
            });
        }
        defaults.sort_by_key(|default| default.policy_id);
        Ok(defaults)
    }

    /// Adds or replaces one canonical policy rule. NetworkIntent is not
    /// consulted or written; endpoint ownership establishes realm context.
    pub async fn upsert_policy_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
        policy: PolicyIntent,
    ) -> Result<PolicyIntent, NetworkError> {
        let _guard = self.lock().await;
        if policy.endpoint_id.is_nil() {
            return Err(NetworkError::InvalidRequest);
        }
        validate_policy_shape(&policy)?;
        let endpoint = self
            .inner
            .repository
            .get_canonical_endpoint(project_id, &policy.endpoint_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &endpoint.realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::InvalidRequest)?;
        if realm.network_id != network_id || realm.state != "active" {
            return Err(NetworkError::Conflict);
        }
        self.inner
            .repository
            .upsert_canonical_policy(&canonical_policy_record(project_id, &policy))
            .await
            .map_err(map_store_error)?;
        Ok(policy)
    }

    pub async fn delete_policy_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
        policy_id: Uuid,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        let exists = self
            .list_policies_for_project(project_id, network_id)
            .await?
            .iter()
            .any(|policy| policy.id == policy_id);
        if !exists {
            return Err(NetworkError::NotFound);
        }
        self.inner
            .repository
            .delete_canonical_policy(project_id, &policy_id)
            .await
            .map_err(map_store_error)
    }

    /// Compatibility hook retained for callers that report provider
    /// realization. Canonical Network state is authoritative; this hook must
    /// not mutate the transitional NetworkIntent payload.
    pub async fn mark_network_intent_active_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
    ) -> Result<(), NetworkError> {
        self.get_canonical_network_for_project(project_id, network_id)
            .await?;
        Ok(())
    }

    pub async fn list_networks(
        &self,
        auth: &AuthContext,
    ) -> Result<Vec<NetworkRecord>, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "ListNetworks").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "ListNetworks".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("network", "network")
                    .map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::Unauthorized);
        }
        self.list_networks_for_project(auth.effective_scope().id().as_str())
            .await
    }

    pub async fn list_networks_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<NetworkRecord>, NetworkError> {
        self.inner
            .repository
            .list_canonical_networks(project_id)
            .await
            .map(|networks| {
                networks
                    .into_iter()
                    .map(canonical_network_projection)
                    .collect()
            })
            .map_err(map_store_error)
    }

    pub async fn get_network(
        &self,
        auth: &AuthContext,
        id: Uuid,
    ) -> Result<NetworkRecord, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "ReadNetwork").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "ReadNetwork".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("network", "network")
                    .map_err(|_| NetworkError::InvalidRequest)?,
                ResourceId::new(id.to_string()).map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::NotFound);
        }
        self.get_network_for_project(auth.effective_scope().id().as_str(), id)
            .await
    }

    pub async fn get_network_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<NetworkRecord, NetworkError> {
        self.inner
            .repository
            .get_canonical_network(project_id, &id)
            .await
            .map(|network| network.map(canonical_network_projection))
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)
    }

    pub async fn update_network(
        &self,
        auth: &AuthContext,
        id: Uuid,
        name: Option<String>,
        admin_state_up: Option<bool>,
    ) -> Result<NetworkRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(auth, "UpdateNetwork", "network", Some(id), None)
            .await?;
        if name.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(NetworkError::InvalidRequest);
        }
        let project_id = auth.effective_scope().id().as_str();
        let current = self
            .inner
            .repository
            .get_canonical_network(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        let name = name.unwrap_or_else(|| current.name.clone());
        let admin_state_up = admin_state_up.unwrap_or(current.admin_state_up);
        let result = self
            .inner
            .repository
            .update_canonical_network(project_id, &id, current.generation, &name, admin_state_up)
            .await
            .map(canonical_network_projection)
            .map_err(map_store_error);
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            Some(id),
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn delete_network(&self, auth: &AuthContext, id: Uuid) -> Result<(), NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "DeleteNetwork").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "DeleteNetwork".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("network", "network")
                    .map_err(|_| NetworkError::InvalidRequest)?,
                ResourceId::new(id.to_string()).map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::NotFound);
        }
        match self
            .delete_network_for_project(auth.effective_scope().id().as_str(), id)
            .await
        {
            Ok(()) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("network", "network").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("network".to_owned(), "network".to_owned())
                        }),
                        ResourceId::new(id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(())
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn delete_network_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .delete_canonical_network(project_id, &id)
            .await
            .map_err(map_store_error)?;
        let _ = self.inner.repository.delete_network(project_id, &id).await;
        let _ = self
            .inner
            .repository
            .release_reservation_for_operation(&format!("o3k:network:create:{}:{}", project_id, id))
            .await;
        Ok(())
    }
}
