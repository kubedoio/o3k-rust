use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs, io,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use o3k_domain::{
    AddressRealm, Ipv4Prefix, NamespacedRoutedFabricPlan, NetworkCapability, NetworkIntent,
    NetworkPlanIntent, NetworkProtocol, PolicyAction, PolicyDefaultIntent, PolicyDirection,
    PolicyIntent, PolicyStatefulMode, PortRange,
};
use o3k_kernel::{
    ActionId, AuditEvent, AuditOutcome, AuditSink, AuthContext, AuthorizationRequest, Authorizer,
    DecisionReason, LimitKey, LimitValue, NoopAuditSink, OwnershipScope, ResourceAmount,
    ResourceId, ResourceTarget, ResourceType, ScopeId, ServiceNamespace, StaticAuthorizer,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub mod canonical_policy;
pub mod execution;
pub mod fabric;
pub mod gateway;
pub mod linux_fabric;
pub mod policy;
pub mod public;
pub use policy::{PolicyEndpoint, PolicyNetworkError, StatefulPolicyProvider};
pub mod routed;
pub use canonical_policy::{
    CanonicalPolicyCompileError, CanonicalPolicyService, CanonicalPolicyServiceError,
    LinuxPolicySnapshotRealizer, PolicyApplyOutcome, PolicyObservation, PolicySnapshotRealizer,
    compile_endpoint_policy,
};
pub use execution::{
    FlatNetworkError, FlatNetworkRealizer, NetworkAgentIdentity, NetworkControllerLease,
    NetworkDispatchError, NetworkExecutionError, NetworkPlanAction, NetworkPlanCommand,
    NetworkPlanDispatcher, NetworkPlanExecutor, NetworkPlanRealizer, NetworkPlanStatus,
    PlanAdmission, journal_path,
};
pub use fabric::{FabricBackend, FabricError, FabricRealizer, InMemoryFabricBackend};
pub use gateway::{
    InMemoryL3GatewayBackend, L3GatewayBackend, L3GatewayError, L3GatewayRealizer,
    LinuxL3GatewayProvider, RealmExecutionContext, compile_l3_gateway_execution_plan,
    gateway_plan_fingerprint,
};
pub use linux_fabric::{LinuxFabricBackend, LinuxFabricConfig, LinuxFabricError};
pub use public::{
    PublicAddressAllocator, PublicAddressBinding, PublicAddressError, PublicAddressPool,
    PublicAddressRealizer,
};
pub use routed::{LinuxRoutedProvider, RoutedExternalConfig, RoutedNetworkError};

/// Poll interval while waiting for a freshly created TAP address to settle.
#[cfg(not(test))]
const TAP_ADDRESS_POLL_INTERVAL: Duration = Duration::from_millis(100);
#[cfg(test)]
const TAP_ADDRESS_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// How long the kernel TAP address must continuously match the requested
/// address before it is considered stable. An asynchronously applied udev
/// MAC policy lands within tens of milliseconds of the device add event, so
/// a 200 ms observation window covers it with an order of magnitude margin.
#[cfg(not(test))]
const TAP_ADDRESS_SETTLE_WINDOW: Duration = Duration::from_millis(200);
#[cfg(test)]
const TAP_ADDRESS_SETTLE_WINDOW: Duration = Duration::ZERO;

/// Upper bound for address stabilization before the TAP is rolled back.
const TAP_ADDRESS_STABILIZE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostNetworkConfig {
    pub bridge_name: String,
    pub uplink: Option<String>,
}

/// Optional kernel TAP access identity for consumers such as libvirt that
/// open a pre-created `managed="no"` interface themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapAccess {
    pub user: String,
    pub group: String,
}

/// Stable shared vocabulary for the P9 control-plane boundary. These values
/// are used by authorization, quota, audit, and compatibility projections;
/// provider-native names never become part of this vocabulary.
pub struct NetworkVocabulary;

impl NetworkVocabulary {
    pub fn actions() -> Vec<ActionId> {
        [
            "CreateNetworkIntent",
            "ReadNetworkIntent",
            "UpdateNetworkIntent",
            "DeleteNetworkIntent",
            "AllocateAddress",
            "ReleaseAddress",
        ]
        .into_iter()
        .map(|action| ActionId::new_unchecked("network", action))
        .collect()
    }

    pub fn resources() -> Vec<ResourceType> {
        ["network_intent", "endpoint", "address_allocation"]
            .into_iter()
            .map(|name| ResourceType::new_unchecked("network", name))
            .collect()
    }

    pub fn quota_keys() -> Vec<LimitKey> {
        ["networks", "ports", "address_allocations"]
            .into_iter()
            .map(|name| LimitKey::new_unchecked(ServiceNamespace::network(), name.to_owned()))
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod vocabulary_tests {
    use super::NetworkVocabulary;

    #[test]
    fn p9_vocabulary_is_typed_and_stable() {
        assert_eq!(NetworkVocabulary::actions().len(), 6);
        assert_eq!(NetworkVocabulary::resources().len(), 3);
        assert_eq!(NetworkVocabulary::quota_keys().len(), 3);
        assert_eq!(
            NetworkVocabulary::actions()[0].to_string(),
            "network:CreateNetworkIntent"
        );
        assert_eq!(
            NetworkVocabulary::resources()[0].to_string(),
            "network:network_intent"
        );
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod p9_plan_tests {
    use super::*;
    use o3k_domain::{
        EndpointIntent, EndpointLocation, FabricEndpointRoute, FabricPeer, FabricProviderKind,
        Ipv4Prefix, NamespacedRoutedFabricPlan, NetworkPlanIntent, NetworkProtocol, PolicyAction,
        PolicyDirection, PolicyIntent, PortRange, RealmEncapsulationBinding, RouteIntent,
    };
    use std::net::Ipv4Addr;

    fn prefix(value: &str, length: u8) -> Ipv4Prefix {
        Ipv4Prefix::new(value.parse().expect("test address"), length).expect("test prefix")
    }

    fn capabilities() -> HashSet<NetworkCapability> {
        [
            NetworkCapability::Ipv4,
            NetworkCapability::EndpointAttachment,
            NetworkCapability::Routing,
            NetworkCapability::StatefulPolicy,
        ]
        .into_iter()
        .collect()
    }

    fn overlapping_capabilities() -> HashSet<NetworkCapability> {
        let mut capabilities = capabilities();
        capabilities.insert(NetworkCapability::OverlappingAddressRealms);
        capabilities.insert(NetworkCapability::EncapsulationModes);
        capabilities
    }

    fn intent() -> NetworkIntent {
        let id = Uuid::from_u128(1);
        NetworkIntent {
            id,
            project_id: "project-a".to_owned(),
            realm: AddressRealm {
                id: Uuid::from_u128(2),
                network_id: id,
                project_id: "project-a".to_owned(),
                prefix: prefix("10.0.0.0", 24),
                overlapping_prefixes: false,
            },
            address_pools: vec![],
            endpoints: vec![EndpointIntent {
                id: Uuid::from_u128(3),
                project_id: "project-a".to_owned(),
                realm_id: Uuid::from_u128(2),
                mac: "02:00:00:00:00:03".to_owned(),
                fixed_ip: Ipv4Addr::new(10, 0, 0, 3),
                generation: 4,
            }],
            routes: vec![RouteIntent {
                destination: prefix("0.0.0.0", 0),
                next_hop: Some(Ipv4Addr::new(10, 0, 0, 1)),
            }],
            gateways: vec![],
            egress: vec![],
            public_addresses: vec![],
            policies: vec![PolicyIntent {
                id: Uuid::from_u128(10),
                endpoint_id: Uuid::from_u128(3),
                direction: PolicyDirection::Egress,
                protocol: NetworkProtocol::Tcp,
                ports: Some(PortRange {
                    start: 443,
                    end: 443,
                }),
                source: None,
                destination: Some(prefix("0.0.0.0", 0)),
                action: PolicyAction::Allow,
            }],
            generation: 5,
            state: o3k_domain::NetworkIntentState::Requested,
        }
    }

    #[test]
    fn prefixes_are_canonical_and_overlap_is_explicit() {
        assert!(Ipv4Prefix::new(Ipv4Addr::new(10, 0, 0, 1), 24).is_none());
        let broad = prefix("10.0.0.0", 16);
        let narrow = prefix("10.0.1.0", 24);
        assert!(broad.overlaps(narrow));
        assert!(!prefix("10.1.0.0", 16).overlaps(narrow));
    }

    #[test]
    fn plan_fingerprint_is_deterministic_and_semantic() {
        let value = intent();
        let operation = Uuid::from_u128(6);
        let first =
            compile_node_network_plan(&value, "node-a", operation, 123, &capabilities(), &[])
                .expect("plan");
        let second =
            compile_node_network_plan(&value, "node-a", operation, 123, &capabilities(), &[])
                .expect("plan");
        assert_eq!(first, second);
        assert_eq!(first.plan_id, value.id);
        assert_eq!(first.schema_version, NODE_NETWORK_PLAN_SCHEMA_VERSION);
        assert!(
            first
                .intents
                .iter()
                .any(|item| matches!(item, NetworkPlanIntent::EndpointAttachment { .. }))
        );
        assert_eq!(first.fingerprint_sha256.len(), 64);
    }

    #[test]
    fn fabric_payload_is_fingerprinted_and_admission_validated() {
        let base = compile_node_network_plan(
            &intent(),
            "node-a",
            Uuid::from_u128(6),
            123,
            &capabilities(),
            &[],
        )
        .expect("plan");
        let destination = prefix("10.0.0.3", 32);
        let fabric = NamespacedRoutedFabricPlan {
            local_host: "node-a".to_owned(),
            local_fabric_transport_ip: Ipv4Addr::new(198, 18, 0, 1),
            local_fabric_generation: 2,
            local_underlay_mtu: 1500,
            local_fabric_mtu: 1420,
            realm_id: Uuid::from_u128(2),
            realm_prefix: prefix("10.0.0.0", 24),
            encapsulation: RealmEncapsulationBinding {
                fabric_domain_id: Uuid::from_u128(100),
                realm_id: Uuid::from_u128(2),
                provider_kind: FabricProviderKind::Geneve,
                provider_segment_id: 101,
                binding_generation: 1,
            },
            directory_generation: 3,
            directory: o3k_domain::RealmEndpointDirectory {
                realm_id: Uuid::from_u128(2),
                prefix: prefix("10.0.0.0", 24),
                directory_generation: 3,
                proxy_mac: "02:11:22:33:44:55".to_owned(),
                entries: vec![EndpointLocation {
                    endpoint_id: Uuid::from_u128(3),
                    project_id: "project-a".to_owned(),
                    realm_id: Uuid::from_u128(2),
                    fixed_ip: Ipv4Addr::new(10, 0, 0, 3),
                    mac: "02:00:00:00:00:03".to_owned(),
                    selected_host: "node-b".to_owned(),
                    endpoint_generation: 4,
                    placement_generation: 5,
                }],
            },
            proxy_mac: "02:11:22:33:44:55".to_owned(),
            tenant_mtu: 1400,
            policy_generation: 1,
            policies: Vec::new(),
            policy_defaults: Vec::new(),
            public_bindings: Vec::new(),
            routes: vec![FabricEndpointRoute {
                realm_id: Uuid::from_u128(2),
                destination,
                endpoint_id: Uuid::from_u128(3),
                target_host: "node-b".to_owned(),
                target_fabric_transport_ip: Ipv4Addr::new(198, 18, 0, 2),
                endpoint_generation: 4,
                placement_generation: 5,
                realm_binding_generation: 1,
                fabric_generation: 6,
            }],
            peers: vec![FabricPeer {
                host_id: "node-b".to_owned(),
                public_key: "public-key".to_owned(),
                underlay_endpoint: "192.0.2.2:65001".to_owned(),
                fabric_transport_ip: Ipv4Addr::new(198, 18, 0, 2),
                fabric_generation: 6,
            }],
        };
        let plan = base.clone().with_fabric(fabric).expect("valid P11 plan");
        assert_ne!(plan.fingerprint_sha256, base.fingerprint_sha256);
        assert_eq!(plan.validate_fabric(), Ok(()));

        let mut invalid = plan.clone();
        invalid.fabric.as_mut().expect("fabric").routes[0].destination = prefix("10.0.0.0", 24);
        assert_eq!(
            invalid.validate_fabric(),
            Err(NetworkPlanError::InvalidFabricPlan)
        );
    }

    #[test]
    fn equivalent_retry_with_new_deadline_keeps_plan_fingerprint() {
        let value = intent();
        let operation = Uuid::from_u128(6);
        let first =
            compile_node_network_plan(&value, "node-a", operation, 123, &capabilities(), &[])
                .expect("plan");
        let retried =
            compile_node_network_plan(&value, "node-a", operation, 456, &capabilities(), &[])
                .expect("retried plan");
        assert_eq!(first.fingerprint_sha256, retried.fingerprint_sha256);
        assert_eq!(
            canonical_plan_fingerprint(&first).expect("fingerprint"),
            first.fingerprint_sha256
        );
        assert_eq!(
            canonical_plan_fingerprint(&retried).expect("fingerprint"),
            retried.fingerprint_sha256
        );
    }

    #[test]
    fn plan_rejects_overlap_before_provider_mutation() {
        let existing = AddressRealm {
            id: Uuid::from_u128(7),
            network_id: Uuid::from_u128(70),
            project_id: "project-b".to_owned(),
            prefix: prefix("10.0.0.0", 16),
            overlapping_prefixes: false,
        };
        assert_eq!(
            compile_node_network_plan(
                &intent(),
                "node-a",
                Uuid::from_u128(6),
                123,
                &capabilities(),
                &[existing],
            ),
            Err(NetworkPlanError::OverlappingRealm)
        );
    }

    #[test]
    fn geneve_capability_allows_overlap_only_when_realm_and_provider_opt_in() {
        let existing = AddressRealm {
            id: Uuid::from_u128(7),
            network_id: Uuid::from_u128(70),
            project_id: "project-b".to_owned(),
            prefix: prefix("10.0.0.0", 16),
            overlapping_prefixes: false,
        };
        let mut overlapping = intent();
        overlapping.realm.overlapping_prefixes = true;
        let plan = compile_node_network_plan(
            &overlapping,
            "node-a",
            Uuid::from_u128(6),
            123,
            &overlapping_capabilities(),
            &[existing],
        )
        .expect("overlap-capable provider plan");
        assert!(
            plan.intents
                .iter()
                .any(|item| matches!(item, NetworkPlanIntent::AddressRealm { .. }))
        );

        let mut missing_encapsulation = overlapping_capabilities();
        missing_encapsulation.remove(&NetworkCapability::EncapsulationModes);
        assert_eq!(
            compile_node_network_plan(
                &overlapping,
                "node-a",
                Uuid::from_u128(6),
                123,
                &missing_encapsulation,
                &[AddressRealm {
                    id: Uuid::from_u128(7),
                    network_id: Uuid::from_u128(70),
                    project_id: "project-b".to_owned(),
                    prefix: prefix("10.0.0.0", 16),
                    overlapping_prefixes: false,
                }],
            ),
            Err(NetworkPlanError::OverlappingRealm)
        );
    }

    #[test]
    fn plan_rejects_unsupported_capability_before_mutation() {
        let mut missing = capabilities();
        missing.remove(&NetworkCapability::Routing);
        assert_eq!(
            compile_node_network_plan(&intent(), "node-a", Uuid::from_u128(6), 123, &missing, &[],),
            Err(NetworkPlanError::UnsupportedCapability(
                NetworkCapability::Routing
            ))
        );
    }

    #[test]
    fn plan_carries_gateway_egress_public_binding_and_deadline() {
        let mut value = intent();
        value.gateways.push(o3k_domain::GatewayIntent {
            destination: prefix("0.0.0.0", 0),
            gateway: Ipv4Addr::new(10, 0, 0, 1),
            external: true,
        });
        value.egress.push(o3k_domain::EgressIntent {
            external_realm_id: Uuid::from_u128(8),
            enabled: true,
            nat: true,
        });
        value
            .public_addresses
            .push(o3k_domain::PublicAddressBindingIntent {
                id: Uuid::from_u128(9),
                project_id: "project-a".to_owned(),
                public_address: Ipv4Addr::new(198, 51, 100, 10),
                endpoint_id: Uuid::from_u128(3),
                generation: 6,
            });
        let mut capabilities = capabilities();
        capabilities.insert(NetworkCapability::Nat);
        capabilities.insert(NetworkCapability::PublicAddressRealization);
        let plan = compile_node_network_plan(
            &value,
            "node-a",
            Uuid::from_u128(6),
            456,
            &capabilities,
            &[],
        )
        .expect("plan");
        assert_eq!(plan.deadline_unix_ms, 456);
        assert!(
            plan.intents
                .iter()
                .any(|item| matches!(item, NetworkPlanIntent::Gateway(_)))
        );
        assert!(
            plan.intents
                .iter()
                .any(|item| matches!(item, NetworkPlanIntent::Egress(_)))
        );
        assert!(
            plan.intents
                .iter()
                .any(|item| matches!(item, NetworkPlanIntent::PublicAddressBinding(_)))
        );
    }

    #[test]
    fn canonical_l3_gateway_compiles_connected_realm_intents() {
        let gateway = o3k_store::CanonicalL3GatewayRecord {
            id: Uuid::from_u128(100),
            project_id: "project-a".into(),
            name: "gw".into(),
            external_realm_id: Some(Uuid::from_u128(200)),
            enable_snat: true,
            generation: 1,
            state: "active".into(),
        };
        let realms = vec![
            o3k_store::CanonicalAddressRealmRecord {
                id: Uuid::from_u128(1),
                network_id: Uuid::from_u128(10),
                project_id: "project-a".into(),
                prefix: "10.0.0.0/24".into(),
                overlapping_prefixes: false,
                generation: 1,
                state: "active".into(),
            },
            o3k_store::CanonicalAddressRealmRecord {
                id: Uuid::from_u128(2),
                network_id: Uuid::from_u128(11),
                project_id: "project-a".into(),
                prefix: "10.1.0.0/24".into(),
                overlapping_prefixes: false,
                generation: 1,
                state: "active".into(),
            },
        ];
        let attachments = vec![
            o3k_store::CanonicalL3GatewayAttachmentRecord {
                id: Uuid::from_u128(3),
                gateway_id: gateway.id,
                realm_id: realms[0].id,
                project_id: "project-a".into(),
                generation: 1,
                state: "active".into(),
            },
            o3k_store::CanonicalL3GatewayAttachmentRecord {
                id: Uuid::from_u128(4),
                gateway_id: gateway.id,
                realm_id: realms[1].id,
                project_id: "project-a".into(),
                generation: 1,
                state: "active".into(),
            },
        ];
        let compiled =
            compile_l3_gateway_intents(&gateway, &attachments, &realms, &BTreeMap::new())
                .expect("gateway plan");
        assert_eq!(compiled.len(), 2);
        assert_eq!(
            compiled[&realms[0].id].0[0].destination.network,
            Ipv4Addr::new(10, 1, 0, 0)
        );
        assert_eq!(
            compiled[&realms[0].id].1[0].external_realm_id,
            Uuid::from_u128(200)
        );
    }

    #[test]
    fn plan_rejects_nat_without_provider_capability() {
        let mut value = intent();
        value.egress.push(o3k_domain::EgressIntent {
            external_realm_id: Uuid::from_u128(8),
            enabled: true,
            nat: true,
        });
        assert_eq!(
            compile_node_network_plan(
                &value,
                "node-a",
                Uuid::from_u128(6),
                123,
                &capabilities(),
                &[],
            ),
            Err(NetworkPlanError::UnsupportedCapability(
                NetworkCapability::Nat
            ))
        );
    }

    #[test]
    fn plan_rejects_cross_project_endpoint() {
        let mut value = intent();
        value.endpoints[0].project_id = "project-b".to_owned();
        assert_eq!(
            compile_node_network_plan(
                &value,
                "node-a",
                Uuid::from_u128(6),
                123,
                &capabilities(),
                &[],
            ),
            Err(NetworkPlanError::AddressOutsideRealm)
        );
    }

    #[test]
    fn plan_rejects_cross_project_bindings_and_invalid_policies() {
        let mut binding = intent();
        binding
            .public_addresses
            .push(o3k_domain::PublicAddressBindingIntent {
                id: Uuid::from_u128(9),
                project_id: "project-b".to_owned(),
                public_address: Ipv4Addr::new(198, 51, 100, 10),
                endpoint_id: Uuid::from_u128(3),
                generation: 1,
            });
        let mut provider_capabilities = capabilities();
        provider_capabilities.insert(NetworkCapability::PublicAddressRealization);
        assert_eq!(
            compile_node_network_plan(
                &binding,
                "node-a",
                Uuid::from_u128(6),
                123,
                &provider_capabilities,
                &[],
            ),
            Err(NetworkPlanError::OwnershipViolation)
        );

        let mut policy = intent();
        policy.policies[0].endpoint_id = Uuid::from_u128(99);
        assert_eq!(
            compile_node_network_plan(
                &policy,
                "node-a",
                Uuid::from_u128(6),
                123,
                &capabilities(),
                &[],
            ),
            Err(NetworkPlanError::InvalidPolicy)
        );

        let mut invalid_ports = intent();
        invalid_ports.policies[0].ports = Some(PortRange {
            start: 5000,
            end: 80,
        });
        assert_eq!(
            compile_node_network_plan(
                &invalid_ports,
                "node-a",
                Uuid::from_u128(6),
                123,
                &capabilities(),
                &[],
            ),
            Err(NetworkPlanError::InvalidPolicy)
        );
    }

    #[test]
    fn plan_rejects_foreign_realm_and_pool() {
        let mut value = intent();
        value.realm.project_id = "project-b".to_owned();
        assert_eq!(
            compile_node_network_plan(
                &value,
                "node-a",
                Uuid::from_u128(6),
                123,
                &capabilities(),
                &[],
            ),
            Err(NetworkPlanError::OwnershipViolation)
        );

        let mut value = intent();
        value.address_pools.push(o3k_domain::AddressPool {
            id: Uuid::from_u128(10),
            realm_id: value.realm.id,
            project_id: "project-a".to_owned(),
            prefix: prefix("10.1.0.0", 24),
            gateway: Some(Ipv4Addr::new(10, 1, 0, 1)),
            first_usable: Ipv4Addr::new(10, 1, 0, 2),
            last_usable: Ipv4Addr::new(10, 1, 0, 20),
        });
        assert_eq!(
            compile_node_network_plan(
                &value,
                "node-a",
                Uuid::from_u128(6),
                123,
                &capabilities(),
                &[],
            ),
            Err(NetworkPlanError::InvalidAddressPool)
        );

        let mut value = intent();
        value.realm.prefix = prefix("10.0.0.0", 16);
        value.address_pools = vec![
            o3k_domain::AddressPool {
                id: Uuid::from_u128(12),
                realm_id: value.realm.id,
                project_id: value.project_id.clone(),
                prefix: prefix("10.0.0.0", 24),
                gateway: Some(Ipv4Addr::new(10, 0, 0, 1)),
                first_usable: Ipv4Addr::new(10, 0, 0, 2),
                last_usable: Ipv4Addr::new(10, 0, 0, 20),
            },
            o3k_domain::AddressPool {
                id: Uuid::from_u128(13),
                realm_id: value.realm.id,
                project_id: value.project_id.clone(),
                prefix: prefix("10.0.1.0", 24),
                gateway: Some(Ipv4Addr::new(10, 0, 1, 130)),
                first_usable: Ipv4Addr::new(10, 0, 1, 130),
                last_usable: Ipv4Addr::new(10, 0, 1, 140),
            },
        ];
        assert_eq!(
            compile_node_network_plan(
                &value,
                "node-a",
                Uuid::from_u128(6),
                123,
                &capabilities(),
                &[],
            ),
            Err(NetworkPlanError::InvalidAddressPool)
        );
    }

    #[test]
    fn plan_rejects_duplicate_endpoint_addresses_and_macs() {
        let mut duplicate_address = intent();
        duplicate_address.endpoints.push(EndpointIntent {
            id: Uuid::from_u128(99),
            project_id: duplicate_address.project_id.clone(),
            realm_id: duplicate_address.realm.id,
            mac: "02:00:00:00:00:99".to_owned(),
            fixed_ip: Ipv4Addr::new(10, 0, 0, 3),
            generation: 1,
        });
        assert_eq!(
            compile_node_network_plan(
                &duplicate_address,
                "node-a",
                Uuid::from_u128(6),
                123,
                &capabilities(),
                &[],
            ),
            Err(NetworkPlanError::ConflictingEndpoint)
        );

        let mut duplicate_mac = intent();
        duplicate_mac.endpoints.push(EndpointIntent {
            id: Uuid::from_u128(99),
            project_id: duplicate_mac.project_id.clone(),
            realm_id: duplicate_mac.realm.id,
            mac: duplicate_mac.endpoints[0].mac.clone(),
            fixed_ip: Ipv4Addr::new(10, 0, 0, 99),
            generation: 1,
        });
        assert_eq!(
            compile_node_network_plan(
                &duplicate_mac,
                "node-a",
                Uuid::from_u128(6),
                123,
                &capabilities(),
                &[],
            ),
            Err(NetworkPlanError::ConflictingEndpoint)
        );

        let mut invalid_mac = intent();
        invalid_mac.endpoints[0].mac = "not-a-mac".to_owned();
        assert_eq!(
            compile_node_network_plan(
                &invalid_mac,
                "node-a",
                Uuid::from_u128(6),
                123,
                &capabilities(),
                &[],
            ),
            Err(NetworkPlanError::ConflictingEndpoint)
        );
    }

    #[test]
    fn plan_rejects_pool_gateway_from_the_allocatable_range() {
        let mut value = intent();
        value.address_pools.push(o3k_domain::AddressPool {
            id: Uuid::from_u128(10),
            realm_id: value.realm.id,
            project_id: value.project_id.clone(),
            prefix: prefix("10.0.0.0", 24),
            gateway: Some(Ipv4Addr::new(10, 0, 0, 1)),
            first_usable: Ipv4Addr::new(10, 0, 0, 1),
            last_usable: Ipv4Addr::new(10, 0, 0, 20),
        });
        assert_eq!(
            compile_node_network_plan(
                &value,
                "node-a",
                Uuid::from_u128(6),
                123,
                &capabilities(),
                &[],
            ),
            Err(NetworkPlanError::InvalidAddressPool)
        );

        let mut value = intent();
        value.address_pools.push(o3k_domain::AddressPool {
            id: Uuid::from_u128(11),
            realm_id: value.realm.id,
            project_id: value.project_id.clone(),
            prefix: prefix("10.0.0.0", 24),
            gateway: Some(Ipv4Addr::new(10, 0, 0, 255)),
            first_usable: Ipv4Addr::new(10, 0, 0, 2),
            last_usable: Ipv4Addr::new(10, 0, 0, 20),
        });
        assert_eq!(
            compile_node_network_plan(
                &value,
                "node-a",
                Uuid::from_u128(6),
                123,
                &capabilities(),
                &[],
            ),
            Err(NetworkPlanError::InvalidAddressPool)
        );
    }

    #[test]
    fn equivalent_plan_replay_is_allowed_but_payload_conflict_is_rejected() {
        let plan = compile_node_network_plan(
            &intent(),
            "node-a",
            Uuid::from_u128(6),
            123,
            &capabilities(),
            &[],
        )
        .expect("plan");
        assert_eq!(validate_plan_replay(&plan, &plan), Ok(()));
        let mut conflicting = plan.clone();
        conflicting.fingerprint_sha256 = "conflicting".to_owned();
        assert_eq!(
            validate_plan_replay(&plan, &conflicting),
            Err(NetworkPlanError::ConflictingPlan)
        );
    }
}

/// The address that O3K is allowed to add to its managed bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewaySpec {
    pub address: Ipv4Addr,
    pub prefix_len: u8,
}

/// Durable ownership metadata for host-local network resources.
///
/// This is deliberately separate from Neutron metadata. It records only
/// resources that this host-network manager may mutate or remove.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct NetworkOwnershipManifest {
    #[serde(default)]
    pub bridge: Option<BridgeOwnership>,
    #[serde(default)]
    pub taps: BTreeMap<String, TapOwnership>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeOwnership {
    pub name: String,
    pub uplink: Option<String>,
    pub created_by_o3k: bool,
    #[serde(default)]
    pub identity: Option<String>,
    #[serde(default)]
    pub gateway: Option<GatewayOwnership>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayOwnership {
    pub address: Ipv4Addr,
    pub prefix_len: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TapOwnership {
    pub interface: String,
    pub instance_id: String,
    pub port_id: String,
    pub mac: String,
    pub bridge: String,
    pub created_by_o3k: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NetworkCommandOutput {
    success: bool,
    stdout: String,
}

trait NetworkCommand: Send + Sync {
    fn output(&self, args: &[&str]) -> io::Result<NetworkCommandOutput>;
    fn status(&self, args: &[&str]) -> io::Result<bool>;
}

struct SystemNetworkCommand;

impl NetworkCommand for SystemNetworkCommand {
    fn output(&self, args: &[&str]) -> io::Result<NetworkCommandOutput> {
        let output = Command::new("ip").args(args).output()?;
        Ok(NetworkCommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        })
    }

    fn status(&self, args: &[&str]) -> io::Result<bool> {
        Ok(Command::new("ip").args(args).status()?.success())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod host_network_tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn validates_names_and_generates_stable_interface_identity() -> Result<(), HostNetworkError> {
        let manager = HostNetworkManager::new(HostNetworkConfig {
            bridge_name: "o3k-br0".to_owned(),
            uplink: None,
        })?;
        assert_eq!(
            HostNetworkManager::tap_name("port-1")?,
            HostNetworkManager::tap_name("port-1")?
        );
        assert_eq!(
            HostNetworkManager::deterministic_mac("port-1")?,
            HostNetworkManager::deterministic_mac("port-1")?
        );
        assert!(matches!(
            HostNetworkManager::new(HostNetworkConfig {
                bridge_name: "../../escape".to_owned(),
                uplink: None
            }),
            Err(HostNetworkError::InvalidName)
        ));
        assert!(matches!(
            manager.create_tap(&TapSpec {
                instance_id: "instance-1".to_owned(),
                port_id: "port-1".to_owned(),
                mac: "bad".to_owned()
            }),
            Err(HostNetworkError::InvalidMac)
        ));
        assert!(matches!(
            manager.delete_tap(&TapSpec {
                instance_id: "instance-1".to_owned(),
                port_id: "port-1".to_owned(),
                mac: "bad".to_owned(),
            }),
            Err(HostNetworkError::InvalidMac)
        ));
        assert!(interface_output_is_owned(
            "2: o3ktap-abcd: <BROADCAST> mtu 1500 master o3k-br0 state UP\\n\\\ttun type tap\\n\\\tlink/ether 02:00:00:00:00:01 brd ff:ff:ff:ff:ff:ff",
            "02:00:00:00:00:01",
            "o3k-br0"
        ));
        assert!(!interface_output_is_owned(
            "2: o3ktap-abcd: <BROADCAST> mtu 1500 master o3k-br0 state UP\\n\\\tlink/ether 02:00:00:00:00:02 brd ff:ff:ff:ff:ff:ff",
            "02:00:00:00:00:01",
            "o3k-br0"
        ));
        Ok(())
    }

    #[test]
    fn managed_bridge_mac_is_stable_and_locally_administered() -> Result<(), HostNetworkError> {
        let first = HostNetworkManager::deterministic_bridge_mac("o3k-b87654403")?;
        let second = HostNetworkManager::deterministic_bridge_mac("o3k-b87654403")?;
        assert_eq!(first, second);
        assert_eq!(first.len(), 17);
        assert!(first.starts_with("02:"));
        assert_ne!(
            first,
            HostNetworkManager::deterministic_bridge_mac("o3k-b87654404")?
        );
        Ok(())
    }

    #[test]
    fn existing_uplink_must_be_up_and_attached_to_the_managed_bridge() {
        let output = "3: eth0: <BROADCAST,UP> mtu 1500 master o3k-br0 state UP";
        assert!(interface_is_attached_to(output, "o3k-br0"));
        assert!(!interface_is_attached_to(
            "3: eth0: <BROADCAST,UP> mtu 1500 state UP",
            "o3k-br0"
        ));
        assert!(!interface_is_attached_to(
            "3: eth0: <BROADCAST> mtu 1500 master o3k-br0 state DOWN",
            "o3k-br0"
        ));
    }

    #[test]
    fn existing_link_must_be_a_bridge_before_it_is_reused() {
        assert!(interface_output_is_bridge(
            "3: o3k-br0: <BROADCAST,UP> mtu 1500 state UP\n\tbridge forward_delay 1500 hello_time 200 max_age 2000"
        ));
        assert!(!interface_output_is_bridge(
            "3: o3k-br0: <BROADCAST,UP> mtu 1500 state UP\n\tlink/ether 02:00:00:00:00:01 brd ff:ff:ff:ff:ff:ff"
        ));
        assert!(!interface_output_is_bridge(
            "3: o3k-br0: <BROADCAST,UP> mtu 1500 state UP\n\tbridge-helper foreign-name"
        ));
    }

    #[test]
    fn bridge_creation_failure_removes_only_the_new_bridge() -> Result<(), HostNetworkError> {
        // The bridge is created under a provisional random name and renamed
        // only after the durable record is written (issue #608); an uplink
        // attach failure after the rename must remove only the newly created
        // bridge — never a foreign or record-less link.
        let root = std::env::temp_dir().join(format!("o3k-network-bridge-{}", Uuid::now_v7()));
        let command = FakeNetworkCommand::new([
            Response::output(false, ""), // link show dev o3k-br0: absent
            Response::status(true),      // link add dev <temp> type bridge
            Response::status(true),      // link set dev <temp> up
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // identity probe of the provisional bridge
            Response::status(true),      // link set dev <temp> down
            Response::status(true),      // link set dev <temp> name o3k-br0
            Response::status(true),      // link set dev o3k-br0 up
            Response::status(false),     // link set dev eth0 master o3k-br0: FAILS
            Response::status(true),      // link del dev o3k-br0 (rollback)
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: Some("eth0".to_owned()),
            },
            Arc::new(command.clone()),
            &root,
        )?;

        assert!(matches!(
            manager.ensure_bridge(),
            Err(HostNetworkError::CommandFailed)
        ));
        let calls = command.calls();
        let temp = calls[1][3].clone();
        assert!(temp.starts_with("o3kbm-"));
        assert_eq!(
            calls,
            vec![
                vec!["link", "show", "dev", "o3k-br0"],
                vec!["link", "add", "name", &temp, "type", "bridge"],
                vec!["link", "set", "dev", &temp, "up"],
                vec!["-d", "link", "show", "dev", &temp],
                vec!["link", "set", "dev", &temp, "down"],
                vec!["link", "set", "dev", &temp, "name", "o3k-br0"],
                vec!["link", "set", "dev", "o3k-br0", "up"],
                vec!["link", "set", "dev", "eth0", "master", "o3k-br0"],
                vec!["link", "del", "dev", "o3k-br0"],
            ]
        );
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn provisional_bridge_failure_removes_only_the_provisional_link() {
        // Issue #608: a failure before the rename (identity probe here) must
        // delete the provisional `o3kbm-*` bridge it created and never touch
        // the deterministic name.
        let command = FakeNetworkCommand::new([
            Response::output(false, ""), // link show dev o3k-br0: absent
            Response::status(true),      // link add dev <temp> type bridge
            Response::status(true),      // link set dev <temp> up
            Response::output(false, ""), // identity probe: command failed
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // rollback probe of the provisional bridge
            Response::status(true),      // link del dev <temp>
        ]);
        let manager = test_manager(command.clone(), None);

        assert!(matches!(
            manager.ensure_bridge(),
            Err(HostNetworkError::ForeignInterface)
        ));
        let calls = command.calls();
        let temp = calls[1][3].clone();
        assert!(temp.starts_with("o3kbm-"));
        assert_eq!(
            calls,
            vec![
                vec!["link", "show", "dev", "o3k-br0"],
                vec!["link", "add", "name", &temp, "type", "bridge"],
                vec!["link", "set", "dev", &temp, "up"],
                vec!["-d", "link", "show", "dev", &temp],
                vec!["-d", "link", "show", "dev", &temp],
                vec!["link", "del", "dev", &temp],
            ]
        );
    }

    #[test]
    fn tap_setup_failure_removes_new_tap_and_bridge() {
        let command = FakeNetworkCommand::new([
            Response::output(false, ""), // link show dev o3k-br0: absent
            Response::status(true),      // link add dev <bridge_temp> type bridge
            Response::status(true),      // link set dev <bridge_temp> up
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // identity probe of the provisional bridge
            Response::status(true),      // link set dev <bridge_temp> down
            Response::status(true),      // link set dev <bridge_temp> name o3k-br0
            Response::status(true),      // link set dev o3k-br0 up
            Response::output(false, ""), // link show dev <tap>: absent
            Response::status(true),      // tuntap add dev <tap_temp> mode tap
            Response::status(true),      // link set dev <tap_temp> address
            Response::status(false),     // link set dev <tap_temp> master: FAILS
            Response::output(
                true,
                "2: o3ktap-abcd: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ), // rollback probe of the provisional tap
            Response::status(true),      // link del dev <tap_temp>
        ]);
        let manager = test_manager(command.clone(), None);
        let spec = TapSpec {
            instance_id: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };

        assert!(matches!(
            manager.create_tap(&spec),
            Err(HostNetworkError::RollbackFailed)
        ));
        let tap = HostNetworkManager::tap_name("port-1").expect("valid test tap name");
        let calls = command.calls();
        // Both the bridge and the TAP are created under provisional random
        // names; the deterministic names are only assigned by the final
        // renames (issues #602, #608).
        let bridge_temp = calls[1][3].clone();
        assert!(bridge_temp.starts_with("o3kbm-"));
        let tap_temp = calls
            .iter()
            .find(|args| args.first().is_some_and(|first| first == "tuntap"))
            .and_then(|args| args.get(3))
            .expect("tuntap add call")
            .clone();
        assert!(tap_temp.starts_with("o3ktmp-"));
        assert_eq!(
            calls,
            vec![
                vec!["link", "show", "dev", "o3k-br0"],
                vec!["link", "add", "name", &bridge_temp, "type", "bridge"],
                vec!["link", "set", "dev", &bridge_temp, "up"],
                vec!["-d", "link", "show", "dev", &bridge_temp],
                vec!["link", "set", "dev", &bridge_temp, "down"],
                vec!["link", "set", "dev", &bridge_temp, "name", "o3k-br0"],
                vec!["link", "set", "dev", "o3k-br0", "up"],
                vec!["link", "show", "dev", &tap],
                vec!["tuntap", "add", "dev", &tap_temp, "mode", "tap"],
                vec![
                    "link",
                    "set",
                    "dev",
                    &tap_temp,
                    "address",
                    "02:00:00:00:00:01"
                ],
                vec!["link", "set", "dev", &tap_temp, "master", "o3k-br0"],
                vec!["-d", "link", "show", "dev", &tap_temp],
                vec!["link", "del", "dev", &tap_temp],
            ]
        );
    }

    #[test]
    fn provisional_tap_residue_is_reaped_without_a_manifest_proof() {
        // Issue #602: a create that dies before the ownership record is
        // durable leaves a provisional `o3ktmp-*` link. It is self-identifying
        // residue, so the startup reap deletes it without a manifest proof
        // while deterministic `o3ktap-*` and foreign links stay untouched.
        let command = FakeNetworkCommand::new([
            Response::output(
                true,
                "2: o3ktmp-1a2b3c4d: <BROADCAST> mtu 1500 state DOWN\n\ttun type tap\n\tlink/ether 02:00:00:00:00:09\n\
                 3: o3ktap-live000: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01\n\
                 4: eth0: <BROADCAST,UP> state UP\n\tlink/ether 02:00:00:00:00:02",
            ),
            Response::status(true), // link del dev o3ktmp-1a2b3c4d
        ]);
        let manager = test_manager(command.clone(), None);
        manager.reap_partial_links().expect("partial reap");
        assert_eq!(
            command.calls(),
            vec![
                vec!["-d", "link", "show"],
                vec!["link", "del", "dev", "o3ktmp-1a2b3c4d"],
            ]
        );
    }

    #[test]
    fn provisional_bridge_residue_is_reaped_without_a_manifest_proof() {
        // Issue #608: a create that dies before the ownership record is
        // durable leaves a provisional `o3kbm-*` bridge. It is self-
        // identifying residue, so the startup reap deletes it without a
        // manifest proof while deterministic `o3k-b-*` bridges, foreign
        // links, and `o3kbm-*`-named non-bridge interfaces stay untouched.
        let command = FakeNetworkCommand::new([
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP> mtu 1500 state UP\n\tbridge forward_delay 1500\n\
                 3: o3kbm-5e6f7788: <BROADCAST,UP> mtu 1500 state UP\n\tlink/ether 02:00:00:00:00:09\n\
                 4: o3k-b-2770749: <BROADCAST,UP> mtu 1500 state UP\n\tbridge forward_delay 1500\n\
                 5: o3ktmp-9a8b7c6d: <BROADCAST> mtu 1500 state DOWN\n\ttun type tap\n\tlink/ether 02:00:00:00:00:0a",
            ),
            Response::status(true), // link del dev o3kbm-1a2b3c4d
            Response::status(true), // link del dev o3ktmp-9a8b7c6d
        ]);
        let manager = test_manager(command.clone(), None);
        manager.reap_partial_links().expect("partial reap");
        assert_eq!(
            command.calls(),
            vec![
                vec!["-d", "link", "show"],
                vec!["link", "del", "dev", "o3kbm-1a2b3c4d"],
                vec!["link", "del", "dev", "o3ktmp-9a8b7c6d"],
            ]
        );
    }

    #[test]
    fn crash_between_record_and_rename_is_fully_reaped() -> Result<(), HostNetworkError> {
        // Issue #602 crash window: the create died after record_tap_ownership
        // but before the rename, so the durable record references the final
        // (never created) deterministic name while the provisional link still
        // exists. The dangling record must be cleared without a kernel delete
        // and the provisional link must be reaped; neither half may survive.
        let root = std::env::temp_dir().join(format!("o3k-network-partial-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).map_err(|_| HostNetworkError::CommandFailed)?;
        let tap = HostNetworkManager::tap_name("port-1")?;
        let manifest = NetworkOwnershipManifest {
            bridge: Some(BridgeOwnership {
                name: "o3k-br0".to_owned(),
                uplink: None,
                created_by_o3k: true,
                identity: Some("2".to_owned()),
                gateway: None,
            }),
            taps: [(
                tap.clone(),
                TapOwnership {
                    interface: tap.clone(),
                    instance_id: "server-1".to_owned(),
                    port_id: "port-1".to_owned(),
                    mac: "02:00:00:00:00:01".to_owned(),
                    bridge: "o3k-br0".to_owned(),
                    created_by_o3k: true,
                },
            )]
            .into_iter()
            .collect(),
        };
        fs::write(
            root.join("ownership.json"),
            serde_json::to_vec(&manifest).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(|_| HostNetworkError::CommandFailed)?;
        let command = FakeNetworkCommand::new([
            Response::output(false, ""), // link show dev <final>: absent
            Response::output(
                true,
                "2: o3ktmp-5e6f7788: <BROADCAST> master o3k-br0 state DOWN\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ),
            Response::status(true), // link del dev o3ktmp-5e6f7788
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command.clone()),
            &root,
        )?;
        manager.delete_taps_for_instance("server-1")?;
        manager.reap_partial_links()?;
        let manifest: NetworkOwnershipManifest = serde_json::from_slice(
            &fs::read(root.join("ownership.json")).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(HostNetworkError::CorruptOwnership)?;
        assert!(manifest.taps.is_empty(), "dangling record must be cleared");
        assert_eq!(
            command.calls(),
            vec![
                vec!["link", "show", "dev", &tap],
                vec!["-d", "link", "show"],
                vec!["link", "del", "dev", "o3ktmp-5e6f7788"],
            ]
        );
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn foreign_existing_tap_is_never_deleted() {
        let command = FakeNetworkCommand::new([
            Response::output(
                true,
                "3: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::output(
                true,
                "3: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::status(true),
            Response::output(true, "2: o3ktap-abcd: <BROADCAST>"),
            Response::output(
                true,
                "2: o3ktap-abcd: <BROADCAST> master o3k-br0\\n\\ttun type tap\\n\\tlink/ether 02:00:00:00:00:02",
            ),
        ]);
        let manager = test_manager(command.clone(), None);
        let spec = TapSpec {
            instance_id: "instance-1".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };

        assert!(matches!(
            manager.create_tap(&spec),
            Err(HostNetworkError::ForeignInterface)
        ));
        assert!(
            !command
                .calls()
                .iter()
                .any(|args| args == &["link", "del", "dev", "o3ktap-abcd"])
        );
    }

    #[test]
    fn discovery_only_returns_taps_attached_to_the_configured_bridge() {
        let command = FakeNetworkCommand::new([Response::output(
            true,
            "2: o3ktap-owned: <BROADCAST,UP> mtu 1500 master o3k-br0 state UP\n\
             tun type tap\n\
             3: o3ktap-detached: <BROADCAST,UP> mtu 1500 state UP\n\
             4: o3ktap-foreign: <BROADCAST,UP> mtu 1500 master other-br0 state UP",
        )]);
        let manager = test_manager(command, None);

        assert_eq!(
            manager.discover_managed().expect("discovery succeeds"),
            vec!["o3ktap-owned"]
        );
    }

    #[test]
    fn ownership_tokens_are_matched_without_prefix_collisions() {
        assert!(interface_output_is_owned(
            "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP tun type tap link/ether 02:00:00:00:00:01",
            "02:00:00:00:00:01",
            "o3k-br0"
        ));
        assert!(!interface_output_is_owned(
            "2: o3ktap-owned: <BROADCAST,UP> master o3k-br01 state UP tun type tap link/ether 02:00:00:00:00:01",
            "02:00:00:00:00:01",
            "o3k-br0"
        ));
        assert!(!interface_output_is_owned(
            "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP tun type tap link/ether 02:00:00:00:00:010",
            "02:00:00:00:00:01",
            "o3k-br0"
        ));
        assert!(!interface_output_is_owned(
            "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP link/ether 02:00:00:00:00:01",
            "02:00:00:00:00:01",
            "o3k-br0"
        ));
    }

    #[test]
    fn tap_ownership_binds_instance_across_manager_restart() -> Result<(), HostNetworkError> {
        let root = std::env::temp_dir().join(format!("o3k-network-ownership-{}", Uuid::now_v7()));
        let command = FakeNetworkCommand::new([
            Response::output(false, ""), // link show dev o3k-br0: absent
            Response::status(true),      // link add dev <bridge_temp> type bridge
            Response::status(true),      // link set dev <bridge_temp> up
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // identity probe of the provisional bridge
            Response::status(true),      // link set dev <bridge_temp> down
            Response::status(true),      // link set dev <bridge_temp> name o3k-br0
            Response::status(true),      // link set dev o3k-br0 up
            Response::output(false, ""), // link show dev <tap>: absent
            Response::status(true),      // tuntap add dev <tap_temp> mode tap
            Response::status(true),      // link set dev <tap_temp> address
            Response::status(true),      // link set dev <tap_temp> master
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ), // address stabilization probe
            Response::status(true),      // rename to the deterministic name
            Response::status(true),      // set up
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command),
            &root,
        )?;
        let spec = TapSpec {
            instance_id: "instance-a".to_owned(),
            port_id: "port-a".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };
        let name = manager.create_tap(&spec)?;
        let manifest: NetworkOwnershipManifest = serde_json::from_slice(
            &fs::read(root.join("ownership.json")).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(HostNetworkError::CorruptOwnership)?;
        assert_eq!(manifest.taps[&name].instance_id, "instance-a");

        let reopened_command = FakeNetworkCommand::new([
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::status(true),
            Response::output(true, "2: o3ktap-owned: <BROADCAST,UP>"),
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::status(true),
            Response::output(true, "2: o3ktap-owned: <BROADCAST,UP>"),
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::status(true),
            Response::output(true, "2: o3ktap-owned: <BROADCAST,UP>"),
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ),
        ]);
        let reopened = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(reopened_command),
            &root,
        )?;
        assert_eq!(reopened.create_tap(&spec)?, name);
        assert!(matches!(
            reopened.create_tap(&TapSpec {
                instance_id: "instance-b".to_owned(),
                ..spec
            }),
            Err(HostNetworkError::ForeignInterface)
        ));
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn ensure_tap_recreates_a_recorded_but_absent_tap_and_reuses_a_present_one()
    -> Result<(), HostNetworkError> {
        // Issue #613 blocker A (host reboot): the durable record survives but
        // the ephemeral TAP is gone, while the persisted domain XML still
        // references the deterministic name. The first `ensure_tap` must
        // re-create the TAP under the recorded name (one `tuntap add`, no
        // duplicate record); the second call must verify and reuse the live
        // TAP without creating another one. The same manager serves both
        // calls, exactly like the startup restoration followed by the next
        // retry pass.
        let root = std::env::temp_dir().join(format!("o3k-network-restore-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).map_err(|_| HostNetworkError::CommandFailed)?;
        let tap = HostNetworkManager::tap_name("port-1")?;
        let manifest = NetworkOwnershipManifest {
            bridge: Some(BridgeOwnership {
                name: "o3k-br0".to_owned(),
                uplink: None,
                created_by_o3k: true,
                identity: Some("2".to_owned()),
                gateway: None,
            }),
            taps: [(
                tap.clone(),
                TapOwnership {
                    interface: tap.clone(),
                    instance_id: "server-1".to_owned(),
                    port_id: "port-1".to_owned(),
                    mac: "02:00:00:00:00:01".to_owned(),
                    bridge: "o3k-br0".to_owned(),
                    created_by_o3k: true,
                },
            )]
            .into_iter()
            .collect(),
        };
        fs::write(
            root.join("ownership.json"),
            serde_json::to_vec(&manifest).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(|_| HostNetworkError::CommandFailed)?;
        // First call: bridge exists (recorded identity matches), TAP absent,
        // so the create path runs under the provisional name and renames to
        // the deterministic one.
        let command = FakeNetworkCommand::new([
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // link show dev o3k-br0 (exists)
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // full bridge probe (owned)
            Response::status(true),      // link set dev o3k-br0 up
            Response::output(false, ""), // link show dev <tap>: absent
            Response::status(true),      // tuntap add (provisional name)
            Response::status(true),      // link set dev <temp> address
            Response::status(true),      // link set dev <temp> master
            Response::output(
                true,
                "2: o3ktap-92bdccea: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ), // address stabilization probe
            Response::status(true),      // rename to the deterministic name
            Response::status(true),      // link set dev <tap> up
            // Second call: TAP present and owned, so no creation happens.
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // link show dev o3k-br0 (exists)
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // full bridge probe (owned)
            Response::status(true), // link set dev o3k-br0 up
            Response::output(true, "2: o3ktap-92bdccea: <BROADCAST>"), // tap exists
            Response::output(
                true,
                "2: o3ktap-92bdccea: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ), // owned-tap probe
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command.clone()),
            &root,
        )?;
        let spec = TapSpec {
            instance_id: "server-1".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };
        assert_eq!(
            manager.owned_tap_specs_for_instance("server-1")?,
            vec![spec.clone()],
            "the durable record must drive the restoration"
        );
        assert!(
            manager
                .owned_tap_specs_for_instance("server-other")?
                .is_empty(),
            "another instance's records must never be selected"
        );
        assert_eq!(
            manager.ensure_tap(&spec)?,
            (tap.clone(), true),
            "the absent recorded TAP must be re-created"
        );
        assert_eq!(
            manager.ensure_tap(&spec)?,
            (tap.clone(), false),
            "the present owned TAP must be verified and reused, not re-created"
        );
        assert_eq!(
            command
                .calls()
                .iter()
                .filter(|args| args[..2] == ["tuntap", "add"])
                .count(),
            1,
            "exactly one TAP creation may ever be issued for the recorded TAP"
        );
        let manifest: NetworkOwnershipManifest = serde_json::from_slice(
            &fs::read(root.join("ownership.json")).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(HostNetworkError::CorruptOwnership)?;
        assert_eq!(
            manifest.taps.len(),
            1,
            "the restoration must never duplicate the ownership record"
        );
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn ensure_tap_fails_closed_on_a_foreign_link_at_the_recorded_name()
    -> Result<(), HostNetworkError> {
        // Issue #613 blocker A restore path: a FOREIGN link exists at the
        // recorded deterministic TAP name (a TAP attached to the bridge but
        // with a different MAC). `ensure_tap` must fail closed with
        // `ForeignInterface` and issue ZERO mutation commands — no
        // `tuntap add`, no `link del` — so the startup restoration holds
        // the instance's domain start back instead of touching the foreign
        // interface.
        let root = std::env::temp_dir().join(format!("o3k-network-foreign-tap-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).map_err(|_| HostNetworkError::CommandFailed)?;
        let tap = HostNetworkManager::tap_name("port-1")?;
        let manifest = NetworkOwnershipManifest {
            bridge: Some(BridgeOwnership {
                name: "o3k-br0".to_owned(),
                uplink: None,
                created_by_o3k: true,
                identity: Some("2".to_owned()),
                gateway: None,
            }),
            taps: [(
                tap.clone(),
                TapOwnership {
                    interface: tap.clone(),
                    instance_id: "server-1".to_owned(),
                    port_id: "port-1".to_owned(),
                    mac: "02:00:00:00:00:01".to_owned(),
                    bridge: "o3k-br0".to_owned(),
                    created_by_o3k: true,
                },
            )]
            .into_iter()
            .collect(),
        };
        fs::write(
            root.join("ownership.json"),
            serde_json::to_vec(&manifest).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(|_| HostNetworkError::CommandFailed)?;
        let command = FakeNetworkCommand::new([
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // link show dev o3k-br0 (exists)
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // full bridge probe (owned)
            Response::status(true), // link set dev o3k-br0 up
            Response::output(true, "2: o3ktap-92bdccea: <BROADCAST>"), // tap exists
            Response::output(
                true,
                "2: o3ktap-92bdccea: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:02",
            ), // owned-tap probe: foreign MAC at the recorded name
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command.clone()),
            &root,
        )?;
        let spec = TapSpec {
            instance_id: "server-1".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };
        assert!(matches!(
            manager.ensure_tap(&spec),
            Err(HostNetworkError::ForeignInterface)
        ));
        assert!(
            !command
                .calls()
                .iter()
                .any(|args| args[..2] == ["tuntap", "add"]),
            "a foreign link must never trigger a TAP creation"
        );
        assert!(
            !command
                .calls()
                .iter()
                .any(|args| args[..2] == ["link", "del"]),
            "a foreign link must never be deleted"
        );
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn crash_residue_is_enumerated_and_reaped_across_restart() -> Result<(), HostNetworkError> {
        // Issue-87 S3 rerun #5: the create prepared the host network (bridge,
        // TAP, DHCP bindings) and the agent died before defining the domain.
        // The control-plane delete converges through local completion and
        // never dispatches an agent delete, so the residue survives until the
        // agent restart reconciliation enumerates the durable manifest and
        // reaps the recorded network state of the absent instance.
        let root = std::env::temp_dir().join(format!("o3k-network-reap-{}", Uuid::now_v7()));
        let spec = TapSpec {
            instance_id: "server-1".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };
        let tap = HostNetworkManager::tap_name("port-1").expect("valid test tap name");
        let first = FakeNetworkCommand::new([
            Response::output(false, ""), // bridge absent
            Response::status(true),      // bridge add (provisional name)
            Response::status(true),      // bridge up (provisional name)
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // identity probe of the provisional bridge
            Response::status(true),      // bridge down (provisional name)
            Response::status(true),      // rename to the deterministic bridge name
            Response::status(true),      // bridge up (deterministic name)
            Response::output(false, ""), // tap absent
            Response::status(true),      // tuntap add (provisional name)
            Response::status(true),      // set address
            Response::status(true),      // set master
            Response::output(
                true,
                &format!(
                    "2: {tap}: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01"
                ),
            ),
            Response::status(true), // rename to the deterministic name
            Response::status(true), // set up
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(first),
            &root,
        )?;
        assert_eq!(manager.create_tap(&spec)?, tap);
        // The agent process is killed here; the kernel and the ownership
        // manifest keep the bridge and TAP with no delete command in flight.

        // On restart the same ownership root is reopened and the kernel still
        // reports the TAP attached to the managed bridge.
        let reopened_command = FakeNetworkCommand::new([
            Response::output(
                true,
                &format!("2: {tap}: <BROADCAST,UP> mtu 1500 master o3k-br0 state UP"),
            ),
            Response::output(
                true,
                &format!(
                    "2: {tap}: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01"
                ),
            ),
            Response::status(true), // link del tap
            Response::output(true, "2: o3k-br0: <BROADCAST,UP> mtu 1500 state UP"),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::status(true), // link del bridge
        ]);
        let reopened = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(reopened_command.clone()),
            &root,
        )?;
        // The restart reconciliation enumerates the residue through the
        // durable manifest and tears down the recorded network state.
        assert_eq!(reopened.owned_instance_ids()?, vec!["server-1".to_owned()]);
        reopened.delete_taps_for_instance("server-1")?;
        reopened.cleanup_if_unused()?;
        let calls = reopened_command.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|args| args.as_slice() == ["link", "del", "dev", &tap])
                .count(),
            1,
            "the TAP must be deleted exactly once"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|args| args.as_slice() == ["link", "del", "dev", "o3k-br0"])
                .count(),
            1,
            "the owned bridge must be deleted exactly once"
        );
        let manifest: NetworkOwnershipManifest = serde_json::from_slice(
            &fs::read(root.join("ownership.json")).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(HostNetworkError::CorruptOwnership)?;
        assert!(manifest.bridge.is_none() && manifest.taps.is_empty());
        // A repeat of the reap after the teardown is a no-op: the manifest is
        // the authority, so no further host command may be issued.
        let calls_before = reopened_command.calls().len();
        reopened.delete_taps_for_instance("server-1")?;
        reopened.cleanup_if_unused()?;
        assert_eq!(reopened_command.calls().len(), calls_before);
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn reaping_one_instance_keeps_the_shared_bridge_until_the_last_tap_is_gone()
    -> Result<(), HostNetworkError> {
        // Issue-87: the managed bridge is shared; reaping one absent instance
        // must never remove it while another recorded instance still uses it.
        let root = std::env::temp_dir().join(format!("o3k-network-shared-{}", Uuid::now_v7()));
        let tap_a = HostNetworkManager::tap_name("port-a").expect("valid test tap name");
        let tap_b = HostNetworkManager::tap_name("port-b").expect("valid test tap name");
        fs::create_dir_all(&root).map_err(|_| HostNetworkError::CommandFailed)?;
        let manifest = NetworkOwnershipManifest {
            bridge: Some(BridgeOwnership {
                name: "o3k-br0".to_owned(),
                uplink: None,
                created_by_o3k: true,
                identity: Some("2".to_owned()),
                gateway: None,
            }),
            taps: [
                (
                    tap_a.clone(),
                    TapOwnership {
                        interface: tap_a.clone(),
                        instance_id: "server-1".to_owned(),
                        port_id: "port-a".to_owned(),
                        mac: "02:00:00:00:00:01".to_owned(),
                        bridge: "o3k-br0".to_owned(),
                        created_by_o3k: true,
                    },
                ),
                (
                    tap_b.clone(),
                    TapOwnership {
                        interface: tap_b.clone(),
                        instance_id: "server-2".to_owned(),
                        port_id: "port-b".to_owned(),
                        mac: "02:00:00:00:00:02".to_owned(),
                        bridge: "o3k-br0".to_owned(),
                        created_by_o3k: true,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        };
        fs::write(
            root.join("ownership.json"),
            serde_json::to_vec_pretty(&manifest).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(|_| HostNetworkError::CommandFailed)?;
        let command = FakeNetworkCommand::new([
            Response::output(
                true,
                &format!("2: {tap_a}: <BROADCAST,UP> mtu 1500 master o3k-br0 state UP"),
            ),
            Response::output(
                true,
                &format!(
                    "2: {tap_a}: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01"
                ),
            ),
            Response::status(true), // link del tap-a
            Response::output(
                true,
                &format!("2: {tap_b}: <BROADCAST,UP> mtu 1500 master o3k-br0 state UP"),
            ),
            Response::output(
                true,
                &format!(
                    "2: {tap_b}: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:02"
                ),
            ),
            Response::status(true), // link del tap-b
            Response::output(true, "2: o3k-br0: <BROADCAST,UP> mtu 1500 state UP"),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::status(true), // link del bridge
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command.clone()),
            &root,
        )?;
        manager.delete_taps_for_instance("server-1")?;
        manager.cleanup_if_unused()?;
        let mid: NetworkOwnershipManifest = serde_json::from_slice(
            &fs::read(root.join("ownership.json")).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(HostNetworkError::CorruptOwnership)?;
        assert!(
            mid.bridge.is_some(),
            "the shared bridge must survive the first reap"
        );
        assert_eq!(mid.taps.len(), 1);
        manager.delete_taps_for_instance("server-2")?;
        manager.cleanup_if_unused()?;
        let end: NetworkOwnershipManifest = serde_json::from_slice(
            &fs::read(root.join("ownership.json")).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(HostNetworkError::CorruptOwnership)?;
        assert!(end.bridge.is_none() && end.taps.is_empty());
        assert_eq!(
            command
                .calls()
                .iter()
                .filter(|args| args.as_slice() == ["link", "del", "dev", "o3k-br0"])
                .count(),
            1,
            "the bridge must be deleted exactly once, after the last TAP"
        );
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn reaping_a_never_prepared_instance_is_a_noop() -> Result<(), HostNetworkError> {
        let root = std::env::temp_dir().join(format!("o3k-network-noop-{}", Uuid::now_v7()));
        let command = FakeNetworkCommand::new([]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command.clone()),
            &root,
        )?;
        assert!(manager.owned_instance_ids()?.is_empty());
        manager.delete_taps_for_instance("never-prepared")?;
        manager.cleanup_if_unused()?;
        assert!(
            command.calls().is_empty(),
            "a never-prepared instance must not touch the host network"
        );
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn tap_address_is_reapplied_after_external_replacement() -> Result<(), HostNetworkError> {
        // A udev MAC policy write can land after the address was set during
        // TAP creation. The owner must observe the replacement, re-apply the
        // requested address, and only then record ownership.
        let command = FakeNetworkCommand::new([
            Response::output(false, ""), // link show dev o3k-br0: absent
            Response::status(true),      // link add dev <bridge_temp> type bridge
            Response::status(true),      // link set dev <bridge_temp> up
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // identity probe of the provisional bridge
            Response::status(true),      // link set dev <bridge_temp> down
            Response::status(true),      // link set dev <bridge_temp> name o3k-br0
            Response::status(true),      // link set dev o3k-br0 up
            Response::output(false, ""), // link show dev <tap>: absent
            Response::status(true),      // tuntap add dev <tap_temp> mode tap
            Response::status(true),      // link set dev <tap_temp> address
            Response::status(true),      // link set dev <tap_temp> master
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 1a:8d:9b:1f:2f:b5",
            ),
            Response::status(true),
            Response::output(
                true,
                "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
            ),
            Response::status(true),
            Response::status(true),
        ]);
        let manager = test_manager(command.clone(), None);
        let spec = TapSpec {
            instance_id: "instance-1".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };

        let name = manager.create_tap(&spec)?;
        let calls = command.calls();
        // Address setup and stabilization happen under the provisional tap
        // name; the bridge too is created under a provisional name and
        // renamed only after its durable record is written (issues #602,
        // #608).
        let bridge_temp = calls[1][3].clone();
        assert!(bridge_temp.starts_with("o3kbm-"));
        let tap_temp = calls
            .iter()
            .find(|args| args.first().is_some_and(|first| first == "tuntap"))
            .and_then(|args| args.get(3))
            .expect("tuntap add call")
            .clone();
        assert!(tap_temp.starts_with("o3ktmp-"));
        let set_calls = calls
            .iter()
            .filter(|args| {
                args.as_slice()
                    == [
                        "link",
                        "set",
                        "dev",
                        &tap_temp,
                        "address",
                        "02:00:00:00:00:01",
                    ]
            })
            .count();
        assert_eq!(set_calls, 2, "address must be re-applied after replacement");
        assert!(
            calls
                .iter()
                .any(|args| args.as_slice() == ["link", "set", "dev", &tap_temp, "name", &name]),
            "the provisional link must be renamed to the deterministic name"
        );
        assert!(
            calls
                .iter()
                .any(|args| args.as_slice()
                    == ["link", "set", "dev", &bridge_temp, "name", "o3k-br0"]),
            "the provisional bridge must be renamed to the deterministic name"
        );
        Ok(())
    }

    #[test]
    fn tap_address_reapply_failure_rolls_back_owned_resources() {
        let mut responses = vec![
            Response::output(false, ""), // link show dev o3k-br0: absent
            Response::status(true),      // link add dev <bridge_temp> type bridge
            Response::status(true),      // link set dev <bridge_temp> up
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // identity probe of the provisional bridge
            Response::status(true),      // link set dev <bridge_temp> down
            Response::status(true),      // link set dev <bridge_temp> name o3k-br0
            Response::status(true),      // link set dev o3k-br0 up
            Response::output(false, ""), // link show dev <tap>: absent
            Response::status(true),      // tuntap add dev <tap_temp> mode tap
            Response::status(true),      // link set dev <tap_temp> address
            Response::status(true),      // link set dev <tap_temp> master
        ];
        // The kernel view never matches the requested address; the second
        // re-apply fails and the owned TAP and bridge are rolled back.
        responses.push(Response::output(
            true,
            "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 1a:8d:9b:1f:2f:b5",
        ));
        responses.push(Response::status(true));
        responses.push(Response::output(
            true,
            "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 1a:8d:9b:1f:2f:b5",
        ));
        responses.push(Response::status(false));
        responses.push(Response::output(
            true,
            "2: o3ktap-owned: <BROADCAST,UP> master o3k-br0 state UP\n\ttun type tap\n\tlink/ether 02:00:00:00:00:01",
        ));
        // Without a durable bridge identity, rollback preserves the bridge for
        // reconciliation instead of guessing that a same-name replacement is
        // still O3K-owned; the newly-created TAP is still removed.
        responses.push(Response::status(true));
        let command = FakeNetworkCommand::new(responses);
        let manager = test_manager(command.clone(), None);
        let spec = TapSpec {
            instance_id: "instance-1".to_owned(),
            port_id: "port-1".to_owned(),
            mac: "02:00:00:00:00:01".to_owned(),
        };

        assert!(matches!(
            manager.create_tap(&spec),
            Err(HostNetworkError::RollbackFailed)
        ));
        let tap = HostNetworkManager::tap_name("port-1").expect("valid test tap name");
        let calls = command.calls();
        // Setup and stabilization run under the provisional names (issues
        // #602, #608).
        let bridge_temp = calls[1][3].clone();
        assert!(bridge_temp.starts_with("o3kbm-"));
        let tap_temp = calls
            .iter()
            .find(|args| args.first().is_some_and(|first| first == "tuntap"))
            .and_then(|args| args.get(3))
            .expect("tuntap add call")
            .clone();
        assert!(tap_temp.starts_with("o3ktmp-"));
        let reapplies = calls
            .iter()
            .filter(|args| {
                args.as_slice()
                    == [
                        "link",
                        "set",
                        "dev",
                        &tap_temp,
                        "address",
                        "02:00:00:00:00:01",
                    ]
            })
            .count();
        assert!(reapplies >= 2, "address must be re-applied while unstable");
        assert_eq!(
            calls.last(),
            Some(&vec![
                "link".to_owned(),
                "del".to_owned(),
                "dev".to_owned(),
                tap_temp.clone()
            ])
        );
        assert!(
            !calls.iter().any(|args| args.len() > 3
                && args[3] == tap
                && (args[0] == "tuntap" || (args[0] == "link" && args[1] != "show"))),
            "the deterministic name must not be mutated before the rename"
        );
    }

    #[test]
    fn gateway_and_bridge_lifecycle_requires_owned_reverse_order() -> Result<(), HostNetworkError> {
        let root = std::env::temp_dir().join(format!("o3k-network-gateway-{}", Uuid::now_v7()));
        let command = FakeNetworkCommand::new([
            Response::output(false, ""), // link show dev o3k-br0: absent
            Response::status(true),      // link add dev <temp> type bridge
            Response::status(true),      // link set dev <temp> up
            Response::output(
                true,
                "2: o3kbm-1a2b3c4d: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // identity probe of the provisional bridge
            Response::status(true),      // link set dev <temp> down
            Response::status(true),      // link set dev <temp> name o3k-br0
            Response::status(true),      // link set dev o3k-br0 up
            Response::status(true),      // addr replace 192.0.2.1/24 dev o3k-br0
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // remove_gateway ownership probe
            Response::status(true),      // addr del 192.0.2.1/24 dev o3k-br0
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // delete_bridge link_exists
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ), // delete_bridge ownership probe
            Response::status(true),      // link del dev o3k-br0
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command),
            &root,
        )?;
        let gateway = GatewaySpec {
            address: "192.0.2.1"
                .parse()
                .map_err(|_| HostNetworkError::InvalidConfiguration)?,
            prefix_len: 24,
        };
        manager.ensure_gateway(gateway)?;
        assert!(matches!(
            manager.delete_bridge(),
            Err(HostNetworkError::OwnershipConflict)
        ));
        manager.remove_gateway(gateway)?;
        manager.delete_bridge()?;
        assert_eq!(
            fs::read_to_string(root.join("ownership.json"))
                .map_err(|_| HostNetworkError::CommandFailed)?,
            "{\n  \"bridge\": null,\n  \"taps\": {}\n}"
        );
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn gateway_does_not_mutate_an_unowned_existing_bridge() -> Result<(), HostNetworkError> {
        let root = std::env::temp_dir().join(format!("o3k-network-foreign-{}", Uuid::now_v7()));
        let command = FakeNetworkCommand::new([
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
            Response::output(
                true,
                "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
            ),
        ]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command.clone()),
            &root,
        )?;
        assert!(matches!(
            manager.ensure_gateway(GatewaySpec {
                address: "192.0.2.1"
                    .parse()
                    .map_err(|_| HostNetworkError::InvalidConfiguration)?,
                prefix_len: 24,
            }),
            Err(HostNetworkError::ForeignInterface)
        ));
        assert_eq!(command.calls().len(), 2);
        fs::remove_dir_all(root).map_err(|_| HostNetworkError::CommandFailed)?;
        Ok(())
    }

    #[test]
    fn cleanup_preserves_same_name_foreign_bridge_replacement() -> Result<(), HostNetworkError> {
        let root = std::env::temp_dir().join(format!("o3k-network-replaced-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).map_err(|_| HostNetworkError::CommandFailed)?;
        let gateway = GatewaySpec {
            address: "192.0.2.1"
                .parse()
                .map_err(|_| HostNetworkError::InvalidConfiguration)?,
            prefix_len: 24,
        };
        let manifest = NetworkOwnershipManifest {
            bridge: Some(BridgeOwnership {
                name: "o3k-br0".to_owned(),
                uplink: None,
                created_by_o3k: true,
                identity: Some("2".to_owned()),
                gateway: Some(GatewayOwnership {
                    address: gateway.address,
                    prefix_len: gateway.prefix_len,
                }),
            }),
            taps: BTreeMap::new(),
        };
        fs::write(
            root.join("ownership.json"),
            serde_json::to_vec(&manifest).map_err(|_| HostNetworkError::CommandFailed)?,
        )
        .map_err(|_| HostNetworkError::CommandFailed)?;
        let command = FakeNetworkCommand::new([Response::output(
            true,
            "3: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500",
        )]);
        let manager = HostNetworkManager::with_command_and_ownership(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            Arc::new(command.clone()),
            &root,
        )?;
        assert!(matches!(
            manager.remove_gateway(gateway),
            Err(HostNetworkError::ForeignInterface)
        ));
        assert!(
            command
                .calls()
                .iter()
                .all(|args| args.as_slice() != ["addr", "del", "192.0.2.1/24", "dev", "o3k-br0"])
        );
        Ok(())
    }

    #[test]
    fn manifest_accepts_multiple_taps_for_one_instance() -> Result<(), HostNetworkError> {
        let manifest = NetworkOwnershipManifest {
            bridge: Some(BridgeOwnership {
                name: "o3k-br0".to_owned(),
                uplink: None,
                created_by_o3k: true,
                identity: Some("2".to_owned()),
                gateway: None,
            }),
            taps: [
                (
                    "o3ktap-a".to_owned(),
                    TapOwnership {
                        interface: "o3ktap-a".to_owned(),
                        instance_id: "server-1".to_owned(),
                        port_id: "port-a".to_owned(),
                        mac: "02:00:00:00:00:01".to_owned(),
                        bridge: "o3k-br0".to_owned(),
                        created_by_o3k: true,
                    },
                ),
                (
                    "o3ktap-b".to_owned(),
                    TapOwnership {
                        interface: "o3ktap-b".to_owned(),
                        instance_id: "server-1".to_owned(),
                        port_id: "port-b".to_owned(),
                        mac: "02:00:00:00:00:02".to_owned(),
                        bridge: "o3k-br0".to_owned(),
                        created_by_o3k: true,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        };
        validate_manifest(
            &HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: None,
            },
            &manifest,
        )
    }

    #[derive(Clone)]
    struct FakeNetworkCommand {
        responses: Arc<Mutex<VecDeque<Response>>>,
        calls: Arc<Mutex<Vec<Vec<String>>>>,
    }

    #[derive(Clone)]
    enum Response {
        Output(bool, String),
        Status(bool),
    }

    impl Response {
        fn output(success: bool, stdout: &str) -> Self {
            Self::Output(success, stdout.to_owned())
        }

        fn status(success: bool) -> Self {
            Self::Status(success)
        }
    }

    impl FakeNetworkCommand {
        fn new(responses: impl IntoIterator<Item = Response>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into_iter().collect())),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn next(&self, args: &[&str]) -> Response {
            self.calls
                .lock()
                .expect("test calls mutex")
                .push(args.iter().map(|arg| (*arg).to_owned()).collect());
            self.responses
                .lock()
                .expect("test responses mutex")
                .pop_front()
                .expect("test response for every command")
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("test calls mutex").clone()
        }
    }

    impl NetworkCommand for FakeNetworkCommand {
        fn output(&self, args: &[&str]) -> io::Result<NetworkCommandOutput> {
            let identity_probe = args == ["-d", "link", "show", "dev", "o3k-br0"]
                && self
                    .calls
                    .lock()
                    .expect("test calls mutex")
                    .last()
                    .is_some_and(|previous| {
                        previous == &["link", "set", "dev", "o3k-br0", "up"]
                            || (previous.len() >= 2
                                && previous[previous.len() - 2..] == ["master", "o3k-br0"])
                    });
            if identity_probe {
                return Ok(NetworkCommandOutput {
                    success: true,
                    stdout: "2: o3k-br0: <BROADCAST,UP>\n\tbridge forward_delay 1500".to_owned(),
                });
            }
            match self.next(args) {
                Response::Output(success, stdout) => Ok(NetworkCommandOutput { success, stdout }),
                Response::Status(_) => panic!("test output response expected"),
            }
        }

        fn status(&self, args: &[&str]) -> io::Result<bool> {
            if args.len() == 6
                && args[..4] == ["link", "set", "dev", "o3k-br0"]
                && args[4] == "address"
            {
                return Ok(true);
            }
            match self.next(args) {
                Response::Status(success) => Ok(success),
                Response::Output(_, _) => panic!("test status response expected"),
            }
        }
    }

    fn test_manager(command: FakeNetworkCommand, uplink: Option<&str>) -> HostNetworkManager {
        HostNetworkManager::with_command(
            HostNetworkConfig {
                bridge_name: "o3k-br0".to_owned(),
                uplink: uplink.map(str::to_owned),
            },
            Arc::new(command),
        )
        .expect("valid test network configuration")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapSpec {
    pub instance_id: String,
    pub port_id: String,
    pub mac: String,
}

#[derive(Debug, Error)]
pub enum HostNetworkError {
    #[error("host network configuration is invalid")]
    InvalidConfiguration,
    #[error("host network operation failed")]
    CommandFailed,
    #[error("host network interface name is invalid")]
    InvalidName,
    #[error("host network MAC address is invalid")]
    InvalidMac,
    #[error("existing TAP interface is not owned by the requested O3K network")]
    ForeignInterface,
    #[error("host network rollback failed after an operation error")]
    RollbackFailed,
    #[error("host network ownership metadata is corrupt")]
    CorruptOwnership(#[source] serde_json::Error),
    #[error("host network ownership metadata could not be persisted")]
    OwnershipStorage(#[source] io::Error),
    #[error("host network ownership metadata conflicts with the requested resource")]
    OwnershipConflict,
}

impl HostNetworkConfig {
    pub fn validate(&self) -> Result<(), HostNetworkError> {
        validate_ifname(&self.bridge_name)?;
        if let Some(uplink) = &self.uplink {
            validate_ifname(uplink)?;
        }
        Ok(())
    }
}

pub struct HostNetworkManager {
    config: HostNetworkConfig,
    command: Arc<dyn NetworkCommand>,
    ownership: Option<Mutex<OwnershipStore>>,
    set_stable_bridge_mac: bool,
    tap_access: Option<TapAccess>,
}

struct OwnershipStore {
    path: PathBuf,
    manifest: NetworkOwnershipManifest,
}

impl HostNetworkManager {
    pub fn new(config: HostNetworkConfig) -> Result<Self, HostNetworkError> {
        config.validate()?;
        Ok(Self {
            config,
            command: Arc::new(SystemNetworkCommand),
            ownership: None,
            set_stable_bridge_mac: true,
            tap_access: None,
        })
    }

    /// Opens a manager with a durable, manager-owned host resource manifest.
    ///
    /// Existing links are still validated using read-only `ip` metadata. The
    /// manifest is required before O3K will mutate or remove a gateway or
    /// bridge, and it binds each reusable TAP to its instance and port.
    pub fn with_ownership_root(
        config: HostNetworkConfig,
        root: impl Into<PathBuf>,
    ) -> Result<Self, HostNetworkError> {
        config.validate()?;
        let root = root.into();
        fs::create_dir_all(&root).map_err(HostNetworkError::OwnershipStorage)?;
        let path = root.join("ownership.json");
        let manifest = load_ownership(&path)?;
        validate_manifest(&config, &manifest)?;
        Ok(Self {
            config,
            command: Arc::new(SystemNetworkCommand),
            ownership: Some(Mutex::new(OwnershipStore { path, manifest })),
            set_stable_bridge_mac: true,
            tap_access: None,
        })
    }

    #[cfg(test)]
    fn with_command(
        config: HostNetworkConfig,
        command: Arc<dyn NetworkCommand>,
    ) -> Result<Self, HostNetworkError> {
        config.validate()?;
        Ok(Self {
            config,
            command,
            ownership: None,
            set_stable_bridge_mac: false,
            tap_access: None,
        })
    }

    #[cfg(test)]
    fn with_command_and_ownership(
        config: HostNetworkConfig,
        command: Arc<dyn NetworkCommand>,
        root: impl Into<PathBuf>,
    ) -> Result<Self, HostNetworkError> {
        config.validate()?;
        let root = root.into();
        fs::create_dir_all(&root).map_err(HostNetworkError::OwnershipStorage)?;
        let path = root.join("ownership.json");
        let manifest = load_ownership(&path)?;
        validate_manifest(&config, &manifest)?;
        Ok(Self {
            config,
            command,
            ownership: Some(Mutex::new(OwnershipStore { path, manifest })),
            set_stable_bridge_mac: false,
            tap_access: None,
        })
    }

    /// Configures the kernel identity allowed to open newly created TAPs.
    /// This is intentionally explicit and optional; ordinary host consumers
    /// retain the historical root-owned TAP behavior.
    pub fn with_tap_access(mut self, access: Option<TapAccess>) -> Result<Self, HostNetworkError> {
        if access
            .as_ref()
            .is_some_and(|value| value.user.trim().is_empty() || value.group.trim().is_empty())
        {
            return Err(HostNetworkError::InvalidConfiguration);
        }
        self.tap_access = access;
        Ok(self)
    }
    pub fn tap_name(port_id: &str) -> Result<String, HostNetworkError> {
        if port_id.trim().is_empty() {
            return Err(HostNetworkError::InvalidName);
        }
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(port_id.as_bytes());
        let mut suffix = String::with_capacity(8);
        for byte in digest.iter().take(4) {
            use std::fmt::Write as _;
            let _ = write!(&mut suffix, "{byte:02x}");
        }
        Ok(format!("o3ktap-{suffix}"))
    }
    /// Provisional name for a TAP whose ownership record is not yet durable.
    /// The random suffix makes the name self-identifying crash residue: no
    /// manifest record ever references it, no domain ever attaches it, and it
    /// never collides with a deterministic `o3ktap-` name, so startup
    /// reconciliation may delete it without a manifest proof (issue #602).
    fn partial_tap_name() -> String {
        format!("o3ktmp-{}", partial_suffix())
    }
    /// Provisional name for a bridge whose ownership record is not yet
    /// durable. Same self-identifying residue contract as [`Self::partial_tap_name`]:
    /// no manifest record ever references it and it never collides with a
    /// deterministic `o3k-b*` bridge, so startup reconciliation may delete it
    /// without a manifest proof (issue #608).
    fn partial_bridge_name() -> String {
        format!("o3kbm-{}", partial_suffix())
    }
    pub fn deterministic_mac(port_id: &str) -> Result<String, HostNetworkError> {
        if port_id.trim().is_empty() {
            return Err(HostNetworkError::InvalidName);
        }
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(port_id.as_bytes());
        Ok(format!(
            "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            digest[0], digest[1], digest[2], digest[3], digest[4]
        ))
    }

    /// Returns the stable, locally-administered MAC used by a managed bridge.
    ///
    /// Linux may otherwise change a bridge's automatically selected MAC when
    /// the first TAP is enslaved.  Ownership is recorded only after this
    /// address is set, so the identity remains stable across TAP attach and
    /// detach operations and cannot be confused with a same-name replacement.
    pub fn deterministic_bridge_mac(bridge_name: &str) -> Result<String, HostNetworkError> {
        validate_ifname(bridge_name)?;
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(bridge_name.as_bytes());
        Ok(format!(
            "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            digest[0], digest[1], digest[2], digest[3], digest[4]
        ))
    }
    pub fn ensure_bridge(&self) -> Result<(), HostNetworkError> {
        self.ensure_bridge_with_ownership().map(|_| ())
    }

    /// Adds the managed gateway address after proving that the bridge is an
    /// O3K-owned bridge. A pre-existing bridge without a matching manifest is
    /// intentionally not mutated.
    pub fn ensure_gateway(&self, gateway: GatewaySpec) -> Result<(), HostNetworkError> {
        validate_gateway(gateway)?;
        if let Some(recorded) = self.recorded_gateway()?
            && recorded != gateway
        {
            return Err(HostNetworkError::OwnershipConflict);
        }
        let bridge_created = self.ensure_bridge_with_ownership()?;
        if !bridge_created && !self.bridge_is_owned_live()? {
            return Err(HostNetworkError::ForeignInterface);
        }
        let address = format!("{}/{}", gateway.address, gateway.prefix_len);
        if let Err(error) =
            self.run_ip(["addr", "replace", &address, "dev", &self.config.bridge_name])
        {
            let error = if bridge_created {
                self.rollback_bridge(error)
            } else {
                error
            };
            return Err(error);
        }
        if let Err(error) = self.set_gateway_ownership(gateway) {
            let rollback = self.run_ip(["addr", "del", &address, "dev", &self.config.bridge_name]);
            if rollback.is_err() {
                return Err(HostNetworkError::RollbackFailed);
            }
            if bridge_created {
                return Err(self.rollback_bridge(error));
            } else {
                return Err(error);
            }
        }
        Ok(())
    }

    /// Removes only the gateway address recorded in the ownership manifest.
    pub fn remove_gateway(&self, gateway: GatewaySpec) -> Result<(), HostNetworkError> {
        validate_gateway(gateway)?;
        let Some(recorded) = self.recorded_gateway()? else {
            return Ok(());
        };
        if recorded != gateway {
            return Err(HostNetworkError::OwnershipConflict);
        }
        if !self.bridge_is_owned_live()? {
            return Err(HostNetworkError::ForeignInterface);
        }
        let address = format!("{}/{}", gateway.address, gateway.prefix_len);
        self.run_ip(["addr", "del", &address, "dev", &self.config.bridge_name])?;
        self.clear_gateway_ownership()
    }

    /// Deletes the bridge only when O3K created it and no owned TAP remains.
    pub fn delete_bridge(&self) -> Result<(), HostNetworkError> {
        let Some(bridge) = self.recorded_bridge()? else {
            return Err(HostNetworkError::ForeignInterface);
        };
        if !bridge.created_by_o3k || bridge.gateway.is_some() || !self.recorded_taps_empty()? {
            return Err(HostNetworkError::OwnershipConflict);
        }
        if self.link_exists(&self.config.bridge_name) {
            let output =
                self.command_output(["-d", "link", "show", "dev", &self.config.bridge_name])?;
            if !output.success
                || !interface_output_is_bridge(&output.stdout)
                || !self.bridge_is_owned_output(&output)
            {
                return Err(HostNetworkError::ForeignInterface);
            }
            self.run_ip(["link", "del", "dev", &self.config.bridge_name])?;
        }
        self.clear_bridge_ownership()
    }

    fn ensure_bridge_with_ownership(&self) -> Result<bool, HostNetworkError> {
        if self.link_exists(&self.config.bridge_name) {
            let output =
                self.command_output(["-d", "link", "show", "dev", &self.config.bridge_name])?;
            if !output.success || !interface_output_is_bridge(&output.stdout) {
                return Err(HostNetworkError::ForeignInterface);
            }
            if self.ownership.is_some() && !self.bridge_is_owned_output(&output) {
                return Err(HostNetworkError::ForeignInterface);
            }
            self.run_ip(["link", "set", "dev", &self.config.bridge_name, "up"])?;
            if let Some(uplink) = &self.config.uplink {
                let output = self.command_output(["-o", "link", "show", "dev", uplink])?;
                if !output.success {
                    return Err(HostNetworkError::CommandFailed);
                }
                if !interface_is_attached_to(&output.stdout, &self.config.bridge_name) {
                    return Err(HostNetworkError::ForeignInterface);
                }
            }
            return Ok(false);
        }
        // Create under a provisional random name and rename only after the
        // ownership record is durable. A crash before the rename leaves an
        // `o3kbm-*` bridge that no manifest record ever references by that
        // name and that never collides with a deterministic `o3k-b*` bridge,
        // so startup reconciliation can delete it without weakening the
        // foreign-interface fence (issue #608: a crash between link creation
        // and ownership recording otherwise orphaned a deterministic-name
        // bridge that the ownership fence permanently refused and no reap
        // covered).
        let temp_name = Self::partial_bridge_name();
        self.run_ip(["link", "add", "name", &temp_name, "type", "bridge"])?;
        let setup = (|| {
            if self.set_stable_bridge_mac {
                let bridge_mac = Self::deterministic_bridge_mac(&self.config.bridge_name)?;
                self.run_ip(["link", "set", "dev", &temp_name, "address", &bridge_mac])?;
            }
            self.run_ip(["link", "set", "dev", &temp_name, "up"])
        })();
        if let Err(error) = setup {
            return Err(self.rollback_provisional_bridge(&temp_name, error));
        }
        let identity = self
            .command_output(["-d", "link", "show", "dev", &temp_name])
            .ok()
            .filter(|output| output.success && interface_output_is_bridge(&output.stdout))
            .and_then(|output| interface_identity(&output.stdout));
        let Some(identity) = identity else {
            return Err(
                self.rollback_provisional_bridge(&temp_name, HostNetworkError::ForeignInterface)
            );
        };
        // The record is keyed by the deterministic name, so a crash after
        // this point converges on retry exactly like the TAP path.
        if let Err(error) = self.record_bridge_ownership(identity) {
            return Err(self.rollback_provisional_bridge(&temp_name, error));
        }
        // The bridge must be DOWN for the rename; a failure before the
        // rename still removes only the provisional link.
        let renamed = (|| {
            self.run_ip(["link", "set", "dev", &temp_name, "down"])?;
            self.run_ip([
                "link",
                "set",
                "dev",
                &temp_name,
                "name",
                &self.config.bridge_name,
            ])
        })();
        if let Err(error) = renamed {
            return Err(self.rollback_provisional_bridge(&temp_name, error));
        }
        // The uplink is attached only after the rename by the final name, so
        // the recorded master reference is stable. Failures here hit the
        // deterministic rollback: the durable record exists and the live
        // identity is verified before deletion.
        let bring_up = (|| {
            self.run_ip(["link", "set", "dev", &self.config.bridge_name, "up"])?;
            if let Some(uplink) = &self.config.uplink {
                self.run_ip([
                    "link",
                    "set",
                    "dev",
                    uplink,
                    "master",
                    &self.config.bridge_name,
                ])?;
            }
            Ok::<(), HostNetworkError>(())
        })();
        if let Err(error) = bring_up {
            return Err(self.rollback_bridge(error));
        }
        Ok(true)
    }

    pub fn create_tap(&self, spec: &TapSpec) -> Result<String, HostNetworkError> {
        self.ensure_tap(spec).map(|(name, _)| name)
    }

    /// Ensures one owned TAP exists and reports whether this call created it.
    /// Callers use the creation bit to make retries and rollback non-destructive.
    pub fn ensure_tap(&self, spec: &TapSpec) -> Result<(String, bool), HostNetworkError> {
        validate_reference(&spec.instance_id)?;
        validate_reference(&spec.port_id)?;
        validate_mac(&spec.mac)?;
        let bridge_created = self.ensure_bridge_with_ownership()?;
        let name = Self::tap_name(&spec.port_id)?;
        if self.link_exists(&name) {
            if !interface_is_owned_with(&*self.command, &name, &spec.mac, &self.config.bridge_name)?
            {
                if bridge_created {
                    return Err(self.rollback_bridge(HostNetworkError::ForeignInterface));
                }
                return Err(HostNetworkError::ForeignInterface);
            }
            self.validate_recorded_tap(&name, spec)?;
            return Ok((name, false));
        }
        // Create under a provisional random name and rename only after the
        // ownership record is durable. A crash before the rename leaves an
        // `o3ktmp-*` link that no manifest record ever references and that
        // never collides with a deterministic `o3ktap-` name, so startup
        // reconciliation can delete it without weakening the foreign-interface
        // fence (issue #602: a crash between link creation and ownership
        // recording otherwise orphaned a deterministic-name TAP that wedged
        // every later create on the network).
        let temp_name = Self::partial_tap_name();
        let mut tuntap_args = vec!["tuntap", "add", "dev", &temp_name, "mode", "tap"];
        if let Some(access) = &self.tap_access {
            tuntap_args.extend(["user", access.user.as_str(), "group", access.group.as_str()]);
        }
        let created_tap = self.run_ip(tuntap_args);
        if let Err(error) = created_tap {
            return Err(if bridge_created {
                self.rollback_bridge(error)
            } else {
                error
            });
        }
        let setup = (|| {
            self.run_ip(["link", "set", "dev", &temp_name, "address", &spec.mac])?;
            self.run_ip([
                "link",
                "set",
                "dev",
                &temp_name,
                "master",
                &self.config.bridge_name,
            ])?;
            Ok::<(), HostNetworkError>(())
        })();
        if let Err(error) = setup {
            return Err(self.rollback_tap_and_bridge(&temp_name, &spec.mac, bridge_created, error));
        }
        if let Err(error) = self.stabilize_tap_address(&temp_name, &spec.mac) {
            return Err(self.rollback_tap_and_bridge(&temp_name, &spec.mac, bridge_created, error));
        }
        if let Err(error) = self.record_tap_ownership(&name, spec) {
            return Err(self.rollback_tap_and_bridge(&temp_name, &spec.mac, bridge_created, error));
        }
        // The link was never brought up, so the rename is accepted; ownership
        // is already recorded under the final name. From here the recorded
        // startup reap covers a crash exactly as before.
        if let Err(error) = self.run_ip(["link", "set", "dev", &temp_name, "name", &name]) {
            let mut rollback =
                self.rollback_tap_and_bridge(&temp_name, &spec.mac, bridge_created, error);
            if self.clear_tap_ownership(&name, spec).is_err() {
                rollback = HostNetworkError::RollbackFailed;
            }
            return Err(rollback);
        }
        if let Err(error) = self.run_ip(["link", "set", "dev", &name, "up"]) {
            let mut rollback =
                self.rollback_tap_and_bridge(&name, &spec.mac, bridge_created, error);
            if self.clear_tap_ownership(&name, spec).is_err() {
                rollback = HostNetworkError::RollbackFailed;
            }
            return Err(rollback);
        }
        Ok((name, true))
    }
    /// Re-applies the requested TAP address until the kernel view stays
    /// stable across a settle window.
    ///
    /// A udev `net_setup_link` policy (for example the
    /// `MACAddressPolicy=persistent` shipped by `99-default.link`) is applied
    /// when the device add event is processed. That policy decision is based
    /// on attributes read when the worker starts, so the policy write can land
    /// after this process already set the address and silently replace it
    /// with a policy-derived address. The policy write happens once per add
    /// event, so observing the requested address across a settle window and
    /// re-applying it after any replacement converges before ownership is
    /// recorded.
    fn stabilize_tap_address(&self, name: &str, mac: &str) -> Result<(), HostNetworkError> {
        let started = Instant::now();
        let mut stable_since: Option<Instant> = None;
        loop {
            let output = self.command_output(["-d", "link", "show", "dev", name])?;
            if !output.success {
                return Err(HostNetworkError::CommandFailed);
            }
            if has_link_token(&output.stdout, "link/ether", mac) {
                let since = stable_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= TAP_ADDRESS_SETTLE_WINDOW {
                    return Ok(());
                }
            } else {
                stable_since = None;
                self.run_ip(["link", "set", "dev", name, "address", mac])?;
            }
            if started.elapsed() >= TAP_ADDRESS_STABILIZE_TIMEOUT {
                return Err(HostNetworkError::CommandFailed);
            }
            std::thread::sleep(TAP_ADDRESS_POLL_INTERVAL);
        }
    }

    /// Deletes a TAP only after proving its expected MAC and bridge ownership.
    pub fn delete_tap(&self, spec: &TapSpec) -> Result<(), HostNetworkError> {
        validate_reference(&spec.instance_id)?;
        validate_reference(&spec.port_id)?;
        validate_mac(&spec.mac)?;
        let name = Self::tap_name(&spec.port_id)?;
        if self.link_exists(&name) {
            if !interface_is_owned_with(&*self.command, &name, &spec.mac, &self.config.bridge_name)?
            {
                return Err(HostNetworkError::ForeignInterface);
            }
            self.validate_recorded_tap(&name, spec)?;
            self.run_ip(["link", "del", "dev", &name])?;
        }
        self.clear_tap_ownership(&name, spec)?;
        Ok(())
    }

    /// Removes every TAP recorded as owned by one instance. Foreign or
    /// malformed ownership records are never selected for deletion.
    pub fn delete_taps_for_instance(&self, instance_id: &str) -> Result<(), HostNetworkError> {
        validate_reference(instance_id)?;
        let specs = self
            .ownership_snapshot(|manifest| {
                manifest
                    .taps
                    .values()
                    .filter(|record| record.created_by_o3k && record.instance_id == instance_id)
                    .map(|record| TapSpec {
                        instance_id: record.instance_id.clone(),
                        port_id: record.port_id.clone(),
                        mac: record.mac.clone(),
                    })
                    .collect::<Vec<_>>()
            })?
            .unwrap_or_default();
        let mut first_error = None;
        for spec in specs {
            if let Err(error) = self.delete_tap(&spec) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Returns durable port identities owned by an instance before its TAP
    /// records are removed. Coupled host services use these identities for
    /// fixed-lease cleanup.
    pub fn owned_port_ids_for_instance(
        &self,
        instance_id: &str,
    ) -> Result<Vec<String>, HostNetworkError> {
        validate_reference(instance_id)?;
        let Some(ownership) = &self.ownership else {
            return Ok(Vec::new());
        };
        let store = ownership.lock().map_err(|_| {
            HostNetworkError::OwnershipStorage(io::Error::other("ownership lock poisoned"))
        })?;
        Ok(store
            .manifest
            .taps
            .values()
            .filter(|record| record.instance_id == instance_id)
            .map(|record| record.port_id.clone())
            .collect())
    }

    /// Returns the create-time specs of the TAPs recorded as O3K-owned for
    /// one instance. The startup domain restoration (issue #613 blocker A)
    /// re-creates these TAPs after a host reboot: the ephemeral devices are
    /// gone while the persisted domain XML still references them. Foreign or
    /// malformed records are never selected for mutation; `ensure_tap`
    /// re-verifies every returned spec against the manifest and the kernel
    /// before creating or reusing anything.
    pub fn owned_tap_specs_for_instance(
        &self,
        instance_id: &str,
    ) -> Result<Vec<TapSpec>, HostNetworkError> {
        validate_reference(instance_id)?;
        Ok(self
            .ownership_snapshot(|manifest| {
                manifest
                    .taps
                    .values()
                    .filter(|record| record.created_by_o3k && record.instance_id == instance_id)
                    .map(|record| TapSpec {
                        instance_id: record.instance_id.clone(),
                        port_id: record.port_id.clone(),
                        mac: record.mac.clone(),
                    })
                    .collect::<Vec<_>>()
            })?
            .unwrap_or_default())
    }

    /// Returns the distinct instance identities recorded in the ownership
    /// manifest. The agent's restart reconciliation enumerates these to find
    /// host artifacts that may be stale after a crash.
    pub fn owned_instance_ids(&self) -> Result<Vec<String>, HostNetworkError> {
        self.ownership_snapshot(|manifest| {
            let mut ids: Vec<String> = manifest
                .taps
                .values()
                .filter(|record| record.created_by_o3k)
                .map(|record| record.instance_id.clone())
                .collect();
            ids.sort();
            ids.dedup();
            ids
        })
        .map(|ids| ids.unwrap_or_default())
    }

    pub fn discover_managed(&self) -> Result<Vec<String>, HostNetworkError> {
        let output = self.command_output(["-d", "link", "show"])?;
        if !output.success {
            return Err(HostNetworkError::CommandFailed);
        }
        Ok(managed_tap_names(&output.stdout, &self.config.bridge_name))
    }

    /// Deletes provisional `o3ktmp-*` TAPs and `o3kbm-*` bridges. Such a link
    /// is by construction residue of a create that died before the ownership
    /// record became durable: manifest records use the final deterministic
    /// name, so the manifest never references a provisional name, no running
    /// domain ever attaches one, and the random suffix never collides with a
    /// legitimate interface. The deterministic `o3ktap-`/`o3k-b*` foreign-
    /// interface fences are unchanged (issues #602, #608).
    pub fn reap_partial_links(&self) -> Result<(), HostNetworkError> {
        let output = self.command_output(["-d", "link", "show"])?;
        if !output.success {
            return Err(HostNetworkError::CommandFailed);
        }
        let mut first_error = None;
        for name in partial_link_names(&output.stdout) {
            if let Err(error) = self.run_ip(["link", "del", "dev", &name]) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Resolves a live TAP only when the durable ownership manifest and the
    /// current kernel interface both prove the requested instance/port/MAC
    /// binding. A deterministic TAP name alone is not sufficient evidence.
    pub fn resolve_owned_tap(&self, spec: &TapSpec) -> Result<String, HostNetworkError> {
        // The ownership manifest may be written by the bounded network
        // executor after a compute agent has opened its manager. Refresh the
        // durable snapshot before a cross-process read so a valid externally
        // realized TAP is not mistaken for a foreign interface.
        self.refresh_ownership()?;
        validate_reference(&spec.instance_id)?;
        validate_reference(&spec.port_id)?;
        validate_mac(&spec.mac)?;
        if self.ownership.is_none() {
            return Err(HostNetworkError::OwnershipConflict);
        }
        let name = Self::tap_name(&spec.port_id)?;
        if !self.link_exists(&name) {
            return Err(HostNetworkError::CommandFailed);
        }
        if !interface_is_owned_with(&*self.command, &name, &spec.mac, &self.config.bridge_name)? {
            return Err(HostNetworkError::ForeignInterface);
        }
        self.validate_recorded_tap(&name, spec)?;
        Ok(name)
    }

    /// Removes the managed gateway and bridge only when no owned TAP remains.
    /// A bridge without a durable O3K ownership record is never touched.
    pub fn cleanup_if_unused(&self) -> Result<(), HostNetworkError> {
        if !self.recorded_taps_empty()? {
            return Ok(());
        }
        if let Some(gateway) = self.recorded_gateway()? {
            self.remove_gateway(gateway)?;
        }
        if self.recorded_bridge()?.is_some() {
            self.delete_bridge()?
        }
        Ok(())
    }

    pub fn ownership_path(&self) -> Option<PathBuf> {
        self.ownership
            .as_ref()
            .and_then(|store| store.lock().ok().map(|guard| guard.path.clone()))
    }

    /// Reloads the manager-owned manifest after another O3K process has
    /// durably changed it. The atomic manifest replacement makes this read
    /// safe across the network executor and compute agent boundary.
    pub fn refresh_ownership(&self) -> Result<(), HostNetworkError> {
        let Some(ownership) = &self.ownership else {
            return Ok(());
        };
        let path = ownership
            .lock()
            .map_err(|_| HostNetworkError::OwnershipConflict)?
            .path
            .clone();
        let manifest = load_ownership(&path)?;
        validate_manifest(&self.config, &manifest)?;
        let mut guard = ownership
            .lock()
            .map_err(|_| HostNetworkError::OwnershipConflict)?;
        guard.manifest = manifest;
        Ok(())
    }

    /// Returns the configured bridge identity for bounded execution adapters.
    pub fn bridge_name(&self) -> Option<String> {
        Some(self.config.bridge_name.clone())
    }

    fn bridge_is_owned_output(&self, output: &NetworkCommandOutput) -> bool {
        let Some(identity) = interface_identity(&output.stdout) else {
            return false;
        };
        self.recorded_bridge().ok().flatten().is_some_and(|record| {
            record.name == self.config.bridge_name
                && record.created_by_o3k
                && record.identity.as_deref() == Some(identity.as_str())
        })
    }

    fn bridge_is_owned_live(&self) -> Result<bool, HostNetworkError> {
        let output =
            self.command_output(["-d", "link", "show", "dev", &self.config.bridge_name])?;
        Ok(output.success
            && interface_output_is_bridge(&output.stdout)
            && self.bridge_is_owned_output(&output))
    }

    fn recorded_bridge(&self) -> Result<Option<BridgeOwnership>, HostNetworkError> {
        self.ownership_snapshot(|manifest| manifest.bridge.clone())
            .map(|value| value.flatten())
    }

    fn recorded_gateway(&self) -> Result<Option<GatewaySpec>, HostNetworkError> {
        Ok(self
            .recorded_bridge()?
            .and_then(|bridge| bridge.gateway)
            .map(|gateway| GatewaySpec {
                address: gateway.address,
                prefix_len: gateway.prefix_len,
            }))
    }

    fn recorded_taps_empty(&self) -> Result<bool, HostNetworkError> {
        self.ownership_snapshot(|manifest| manifest.taps.is_empty())
            .map(|empty| empty.unwrap_or(true))
    }

    fn record_bridge_ownership(&self, identity: String) -> Result<(), HostNetworkError> {
        self.update_ownership(|manifest| {
            if let Some(existing) = &manifest.bridge
                && (existing.name != self.config.bridge_name
                    || existing.uplink != self.config.uplink)
            {
                return Err(HostNetworkError::OwnershipConflict);
            }
            manifest.bridge = Some(BridgeOwnership {
                name: self.config.bridge_name.clone(),
                uplink: self.config.uplink.clone(),
                created_by_o3k: true,
                identity: Some(identity.clone()),
                gateway: manifest
                    .bridge
                    .as_ref()
                    .and_then(|bridge| bridge.gateway.clone()),
            });
            Ok(())
        })
    }

    fn set_gateway_ownership(&self, gateway: GatewaySpec) -> Result<(), HostNetworkError> {
        self.update_ownership(|manifest| {
            let bridge = manifest
                .bridge
                .as_mut()
                .ok_or(HostNetworkError::ForeignInterface)?;
            if bridge.name != self.config.bridge_name || !bridge.created_by_o3k {
                return Err(HostNetworkError::ForeignInterface);
            }
            bridge.gateway = Some(GatewayOwnership {
                address: gateway.address,
                prefix_len: gateway.prefix_len,
            });
            Ok(())
        })
    }

    fn clear_gateway_ownership(&self) -> Result<(), HostNetworkError> {
        self.update_ownership(|manifest| {
            if let Some(bridge) = manifest.bridge.as_mut() {
                bridge.gateway = None;
            }
            Ok(())
        })
    }

    fn clear_bridge_ownership(&self) -> Result<(), HostNetworkError> {
        self.update_ownership(|manifest| {
            if manifest.taps.is_empty() {
                manifest.bridge = None;
                Ok(())
            } else {
                Err(HostNetworkError::OwnershipConflict)
            }
        })
    }

    fn record_tap_ownership(
        &self,
        interface: &str,
        spec: &TapSpec,
    ) -> Result<(), HostNetworkError> {
        self.update_ownership(|manifest| {
            let record = TapOwnership {
                interface: interface.to_owned(),
                instance_id: spec.instance_id.clone(),
                port_id: spec.port_id.clone(),
                mac: spec.mac.to_ascii_lowercase(),
                bridge: self.config.bridge_name.clone(),
                created_by_o3k: true,
            };
            if let Some(existing) = manifest.taps.get(interface)
                && existing != &record
            {
                return Err(HostNetworkError::OwnershipConflict);
            }
            manifest.taps.insert(interface.to_owned(), record);
            Ok(())
        })
    }

    fn validate_recorded_tap(
        &self,
        interface: &str,
        spec: &TapSpec,
    ) -> Result<(), HostNetworkError> {
        let Some(record) = self
            .ownership_snapshot(|manifest| manifest.taps.get(interface).cloned())?
            .flatten()
        else {
            return if self.ownership.is_some() {
                Err(HostNetworkError::ForeignInterface)
            } else {
                Ok(())
            };
        };
        if record.instance_id != spec.instance_id
            || record.port_id != spec.port_id
            || !record.mac.eq_ignore_ascii_case(&spec.mac)
            || record.bridge != self.config.bridge_name
            || !record.created_by_o3k
        {
            return Err(HostNetworkError::ForeignInterface);
        }
        Ok(())
    }

    fn clear_tap_ownership(&self, interface: &str, spec: &TapSpec) -> Result<(), HostNetworkError> {
        self.update_ownership(|manifest| {
            let Some(record) = manifest.taps.get(interface) else {
                return Ok(());
            };
            if record.instance_id != spec.instance_id
                || record.port_id != spec.port_id
                || !record.mac.eq_ignore_ascii_case(&spec.mac)
            {
                return Err(HostNetworkError::ForeignInterface);
            }
            manifest.taps.remove(interface);
            Ok(())
        })
    }

    fn ownership_snapshot<T>(
        &self,
        read: impl FnOnce(&NetworkOwnershipManifest) -> T,
    ) -> Result<Option<T>, HostNetworkError> {
        let Some(ownership) = &self.ownership else {
            return Ok(None);
        };
        let guard = ownership
            .lock()
            .map_err(|_| HostNetworkError::OwnershipConflict)?;
        Ok(Some(read(&guard.manifest)))
    }

    fn update_ownership(
        &self,
        update: impl FnOnce(&mut NetworkOwnershipManifest) -> Result<(), HostNetworkError>,
    ) -> Result<(), HostNetworkError> {
        let Some(ownership) = &self.ownership else {
            return Ok(());
        };
        let mut guard = ownership
            .lock()
            .map_err(|_| HostNetworkError::OwnershipConflict)?;
        let previous = guard.manifest.clone();
        update(&mut guard.manifest)?;
        if let Err(error) = persist_ownership(&guard.path, &guard.manifest) {
            guard.manifest = previous;
            return Err(error);
        }
        Ok(())
    }

    fn link_exists(&self, name: &str) -> bool {
        self.command
            .output(["link", "show", "dev", name].as_slice())
            .map(|output| output.success)
            .unwrap_or(false)
    }

    fn command_output<'a, I>(&self, args: I) -> Result<NetworkCommandOutput, HostNetworkError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let args = args.into_iter().collect::<Vec<_>>();
        self.command
            .output(&args)
            .map_err(|_| HostNetworkError::CommandFailed)
    }

    fn run_ip<'a, I>(&self, args: I) -> Result<(), HostNetworkError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let args = args.into_iter().collect::<Vec<_>>();
        match self.command.status(&args) {
            Ok(true) => Ok(()),
            Ok(false) | Err(_) => Err(HostNetworkError::CommandFailed),
        }
    }

    fn rollback_bridge(&self, original: HostNetworkError) -> HostNetworkError {
        // A bridge that never reached the durable ownership manifest has no
        // current identity to verify.  Preserve it for reconciliation rather
        // than deleting a same-name replacement during rollback.
        if self.recorded_bridge().ok().flatten().is_none() {
            return HostNetworkError::RollbackFailed;
        }
        let owned_now = self
            .command_output(["-d", "link", "show", "dev", &self.config.bridge_name])
            .ok()
            .is_some_and(|output| {
                output.success
                    && interface_output_is_bridge(&output.stdout)
                    && self.bridge_is_owned_output(&output)
            });
        if owned_now
            && self
                .run_ip(["link", "del", "dev", &self.config.bridge_name])
                .is_ok()
        {
            match self.clear_bridge_ownership() {
                Ok(()) => original,
                Err(_) => HostNetworkError::RollbackFailed,
            }
        } else {
            HostNetworkError::RollbackFailed
        }
    }

    /// Removes a bridge that never reached the durable deterministic name.
    /// The provisional name is O3K-created by construction, so the deletion
    /// guard only has to prove the link is still the bridge we made (its
    /// stable MAC when one was set); a failed deletion leaves a record-less
    /// `o3kbm-*` bridge that the startup reap removes on the next restart
    /// (issue #608).
    fn rollback_provisional_bridge(
        &self,
        temp_name: &str,
        original: HostNetworkError,
    ) -> HostNetworkError {
        let output = self
            .command_output(["-d", "link", "show", "dev", temp_name])
            .ok()
            .filter(|output| output.success && interface_output_is_bridge(&output.stdout));
        let Some(output) = output else {
            return HostNetworkError::RollbackFailed;
        };
        if self.set_stable_bridge_mac {
            let Ok(expected) = Self::deterministic_bridge_mac(&self.config.bridge_name) else {
                return HostNetworkError::RollbackFailed;
            };
            if !has_link_token(&output.stdout, "link/ether", &expected) {
                return HostNetworkError::RollbackFailed;
            }
        }
        if self.run_ip(["link", "del", "dev", temp_name]).is_err() {
            return HostNetworkError::RollbackFailed;
        }
        original
    }

    fn rollback_tap_and_bridge(
        &self,
        tap_name: &str,
        expected_mac: &str,
        bridge_created: bool,
        original: HostNetworkError,
    ) -> HostNetworkError {
        let owned_now = self
            .command_output(["-d", "link", "show", "dev", tap_name])
            .ok()
            .is_some_and(|output| {
                output.success
                    && interface_output_is_owned(
                        &output.stdout,
                        expected_mac,
                        &self.config.bridge_name,
                    )
            });
        if !owned_now || self.run_ip(["link", "del", "dev", tap_name]).is_err() {
            return HostNetworkError::RollbackFailed;
        }
        if bridge_created {
            return self.rollback_bridge(original);
        }
        original
    }
}

fn validate_ifname(name: &str) -> Result<(), HostNetworkError> {
    if name.is_empty()
        || name.len() > 15
        || name
            .bytes()
            .any(|b| !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    {
        return Err(HostNetworkError::InvalidName);
    }
    Ok(())
}

/// Random 8-hex-character suffix from the random tail of a v7 UUID, shared by
/// the provisional TAP (`o3ktmp-`) and bridge (`o3kbm-`) names. 8 hex chars
/// keep either prefixed name inside the 15-byte kernel interface-name limit.
fn partial_suffix() -> String {
    let id = Uuid::now_v7().simple().to_string();
    id[id.len() - 8..].to_owned()
}

fn validate_reference(value: &str) -> Result<(), HostNetworkError> {
    if value.is_empty()
        || value.len() > 128
        || value.chars().any(|character| {
            character.is_whitespace() || character.is_control() || matches!(character, '/' | '\\')
        })
    {
        return Err(HostNetworkError::InvalidName);
    }
    Ok(())
}

fn validate_mac(mac: &str) -> Result<(), HostNetworkError> {
    if mac.len() != 17
        || mac.split(':').count() != 6
        || !mac
            .split(':')
            .all(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(HostNetworkError::InvalidMac);
    }
    Ok(())
}

fn validate_gateway(gateway: GatewaySpec) -> Result<(), HostNetworkError> {
    if gateway.prefix_len > 30 {
        return Err(HostNetworkError::InvalidConfiguration);
    }
    Ok(())
}

fn load_ownership(path: &Path) -> Result<NetworkOwnershipManifest, HostNetworkError> {
    if !path.exists() {
        return Ok(NetworkOwnershipManifest::default());
    }
    serde_json::from_slice(&fs::read(path).map_err(HostNetworkError::OwnershipStorage)?)
        .map_err(HostNetworkError::CorruptOwnership)
}

fn validate_manifest(
    config: &HostNetworkConfig,
    manifest: &NetworkOwnershipManifest,
) -> Result<(), HostNetworkError> {
    if let Some(bridge) = &manifest.bridge {
        if bridge.name != config.bridge_name || bridge.uplink != config.uplink {
            return Err(HostNetworkError::OwnershipConflict);
        }
        if let Some(gateway) = bridge.gateway.as_ref() {
            validate_gateway(GatewaySpec {
                address: gateway.address,
                prefix_len: gateway.prefix_len,
            })?;
        }
    }
    let mut ports = HashSet::new();
    for (interface, tap) in &manifest.taps {
        validate_ifname(interface)?;
        validate_ifname(&tap.interface)?;
        validate_reference(&tap.instance_id)?;
        validate_reference(&tap.port_id)?;
        validate_mac(&tap.mac)?;
        if interface != &tap.interface
            || tap.bridge != config.bridge_name
            || !tap.created_by_o3k
            || !ports.insert(tap.port_id.clone())
        {
            return Err(HostNetworkError::OwnershipConflict);
        }
    }
    Ok(())
}

fn persist_ownership(
    path: &Path,
    manifest: &NetworkOwnershipManifest,
) -> Result<(), HostNetworkError> {
    let temporary = path.with_extension(format!("tmp-{}", Uuid::now_v7()));
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|_| {
        HostNetworkError::OwnershipStorage(io::Error::new(
            io::ErrorKind::InvalidData,
            "ownership metadata serialization failed",
        ))
    })?;
    if let Err(error) = fs::write(&temporary, bytes) {
        let _ = fs::remove_file(&temporary);
        return Err(HostNetworkError::OwnershipStorage(error));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(HostNetworkError::OwnershipStorage(error));
    }
    Ok(())
}

fn interface_is_owned_with(
    command: &dyn NetworkCommand,
    name: &str,
    expected_mac: &str,
    bridge_name: &str,
) -> Result<bool, HostNetworkError> {
    let output = command
        .output(["-d", "link", "show", "dev", name].as_slice())
        .map_err(|_| HostNetworkError::CommandFailed)?;
    if !output.success {
        return Err(HostNetworkError::CommandFailed);
    }
    Ok(interface_output_is_owned(
        &output.stdout,
        expected_mac,
        bridge_name,
    ))
}

fn interface_output_is_owned(output: &str, expected_mac: &str, bridge_name: &str) -> bool {
    interface_output_is_tap(output)
        && has_link_token(output, "link/ether", expected_mac)
        && has_link_token(output, "master", bridge_name)
}

fn interface_output_is_tap(output: &str) -> bool {
    output.contains("tun type tap")
        || output.lines().any(|line| {
            let tokens = line.split_whitespace().collect::<Vec<_>>();
            tokens
                .windows(3)
                .any(|window| window == ["tun", "type", "tap"])
        })
}

fn managed_tap_names(output: &str, bridge_name: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut current_name = None;
    let mut current_output = String::new();
    let finish = |name: &mut Option<String>, block: &mut String, names: &mut Vec<String>| {
        if let Some(name) = name.take()
            && name.starts_with("o3ktap-")
            && interface_output_is_tap(block)
            && interface_is_attached_to(block, bridge_name)
        {
            names.push(name);
        }
        block.clear();
    };
    for line in output.lines() {
        if let Some((_, rest)) = line.split_once(": ")
            && line
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_digit())
            && rest.split(':').next().is_some_and(|name| !name.is_empty())
        {
            finish(&mut current_name, &mut current_output, &mut names);
            current_name = rest.split(':').next().map(str::to_owned);
        }
        if current_name.is_some() {
            current_output.push_str(line);
            current_output.push('\n');
        }
    }
    finish(&mut current_name, &mut current_output, &mut names);
    names
}

fn partial_link_names(output: &str) -> Vec<String> {
    // A provisional link is residue regardless of bridge attachment: a crash
    // can land before `set master`, so no bridge condition applies here. The
    // kernel output proves the link kind: an `o3ktmp-*` name must still be a
    // TAP and an `o3kbm-*` name must still be a bridge. Names come from the
    // kernel; keep only syntactically valid interface names with a
    // provisional prefix.
    let mut names = Vec::new();
    let mut current_name = None;
    let mut current_output = String::new();
    let finish = |name: &mut Option<String>, block: &mut String, names: &mut Vec<String>| {
        if let Some(name) = name.take()
            && validate_ifname(&name).is_ok()
            && ((name.starts_with("o3ktmp-") && interface_output_is_tap(block))
                || (name.starts_with("o3kbm-") && interface_output_is_bridge(block)))
        {
            names.push(name);
        }
        block.clear();
    };
    for line in output.lines() {
        if let Some((_, rest)) = line.split_once(": ")
            && line
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_digit())
            && rest.split(':').next().is_some_and(|name| !name.is_empty())
        {
            finish(&mut current_name, &mut current_output, &mut names);
            current_name = rest.split(':').next().map(str::to_owned);
        }
        if current_name.is_some() {
            current_output.push_str(line);
            current_output.push('\n');
        }
    }
    finish(&mut current_name, &mut current_output, &mut names);
    names
}

fn interface_is_attached_to(output: &str, bridge_name: &str) -> bool {
    output.lines().any(|line| {
        has_link_token(line, "state", "UP") && has_link_token(line, "master", bridge_name)
    })
}

fn has_link_token(output: &str, key: &str, expected: &str) -> bool {
    output
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|pair| pair[0] == key && pair[1].eq_ignore_ascii_case(expected))
}

fn interface_output_is_bridge(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.trim_start().starts_with("bridge "))
}

/// Returns a stable live-link identity from `ip -d link show`: the kernel
/// ifindex plus the link-layer address when present. A missing identity is
/// treated as unowned for destructive operations.
fn interface_identity(output: &str) -> Option<String> {
    let first = output.lines().next()?.trim();
    let index = first.split_once(':')?.0.trim();
    if !index.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mac = output
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|pair| pair[0] == "link/ether")
        .map(|pair| pair[1].to_ascii_lowercase());
    Some(mac.map_or_else(|| index.to_owned(), |mac| format!("{index}:{mac}")))
}

pub use o3k_store::{NetworkRecord, PortRecord, SubnetRecord};

/// A deterministic, provider-independent compilation of canonical network
/// intent. It contains semantic intents only; host commands and provider
/// handles belong behind the execution boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeNetworkPlan {
    pub schema_version: u16,
    pub plan_id: Uuid,
    pub node_id: String,
    pub operation_id: Uuid,
    pub deadline_unix_ms: u64,
    pub resource_generations: BTreeMap<Uuid, u64>,
    pub intents: Vec<NetworkPlanIntent>,
    /// Optional accepted P11 semantic fabric plan. `None` preserves the P9
    /// wire shape and legacy fingerprint for non-P11 plans.
    #[serde(default)]
    pub fabric: Option<NamespacedRoutedFabricPlan>,
    /// Independent multi-Realm gateway execution unit. This is not part of
    /// the Realm-scoped `fabric` plan.
    #[serde(default)]
    pub gateway: Option<o3k_domain::L3GatewayExecutionPlan>,
    pub fingerprint_sha256: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NetworkPlanError {
    #[error("network realm prefix overlaps an existing routed realm")]
    OverlappingRealm,
    #[error("network intent is outside its address realm")]
    AddressOutsideRealm,
    #[error("network intent requires unsupported capability {0:?}")]
    UnsupportedCapability(NetworkCapability),
    #[error("network intent has a conflicting endpoint identity")]
    ConflictingEndpoint,
    #[error("network plan serialization failed")]
    Serialization,
    #[error("network plan identity conflicts with an existing plan")]
    ConflictingPlan,
    #[error("network intent has invalid project ownership")]
    OwnershipViolation,
    #[error("network intent has an invalid address pool")]
    InvalidAddressPool,
    #[error("network intent has an invalid policy")]
    InvalidPolicy,
    #[error("network intent has an invalid IPv4 prefix")]
    InvalidPrefix,
    #[error("P11 fabric plan is invalid")]
    InvalidFabricPlan,
    #[error("L3 gateway execution plan is invalid")]
    InvalidGatewayPlan,
}

pub const NODE_NETWORK_PLAN_SCHEMA_VERSION: u16 = 1;

impl NodeNetworkPlan {
    /// Attaches accepted P11 semantic state and recomputes the transport
    /// fingerprint. Provider-native state is intentionally not accepted here.
    pub fn with_fabric(
        mut self,
        fabric: NamespacedRoutedFabricPlan,
    ) -> Result<Self, NetworkPlanError> {
        self.fabric = Some(fabric);
        self.validate_fabric()?;
        self.fingerprint_sha256 = canonical_plan_fingerprint(&self)?;
        Ok(self)
    }

    /// Attaches the separate provider-independent L3 gateway execution unit.
    pub fn with_gateway(
        mut self,
        gateway: o3k_domain::L3GatewayExecutionPlan,
    ) -> Result<Self, NetworkPlanError> {
        gateway::validate_plan(&gateway).map_err(|_| NetworkPlanError::InvalidGatewayPlan)?;
        self.gateway = Some(gateway);
        self.fingerprint_sha256 = canonical_plan_fingerprint(&self)?;
        Ok(self)
    }

    /// Validates the semantic P11 payload before admission to a node-local
    /// executor. A valid fingerprint alone is insufficient authorization.
    pub fn validate_fabric(&self) -> Result<(), NetworkPlanError> {
        let Some(fabric) = &self.fabric else {
            return Ok(());
        };
        if fabric.local_host != self.node_id
            || fabric.local_host.is_empty()
            || fabric.local_fabric_transport_ip.is_unspecified()
            || fabric.local_fabric_transport_ip.is_loopback()
            || fabric.local_fabric_generation == 0
            || fabric.local_underlay_mtu == 0
            || fabric.local_fabric_mtu == 0
            || fabric.local_fabric_mtu > fabric.local_underlay_mtu
            || fabric.directory_generation == 0
            || fabric.tenant_mtu == 0
            || fabric.tenant_mtu > fabric.local_fabric_mtu
            || fabric.policy_generation == 0
            || fabric.proxy_mac.len() != 17
            || fabric.encapsulation.realm_id != fabric.realm_id
            || fabric.encapsulation.validate().is_err()
            || fabric.directory.realm_id != fabric.realm_id
            || fabric.directory.prefix != fabric.realm_prefix
            || fabric.directory.directory_generation != fabric.directory_generation
            || fabric.directory.proxy_mac != fabric.proxy_mac
        {
            return Err(NetworkPlanError::InvalidFabricPlan);
        }
        if fabric
            .directory
            .entries
            .iter()
            .any(|entry| !fabric.realm_prefix.contains(entry.fixed_ip))
        {
            return Err(NetworkPlanError::InvalidFabricPlan);
        }
        let mut policy_ids = BTreeSet::new();
        for policy in &fabric.policies {
            if policy.id == Uuid::nil()
                || policy.endpoint_id == Uuid::nil()
                || !policy_ids.insert(policy.id)
                || fabric
                    .directory
                    .entries
                    .iter()
                    .all(|entry| entry.endpoint_id != policy.endpoint_id)
            {
                return Err(NetworkPlanError::InvalidFabricPlan);
            }
        }
        let mut default_endpoints = BTreeSet::new();
        for default in &fabric.policy_defaults {
            if default.policy_id.is_nil()
                || default.endpoint_id.is_nil()
                || default.generation == 0
                || default.stateful_mode != o3k_domain::PolicyStatefulMode::Stateful
                || !default_endpoints.insert(default.endpoint_id)
                || fabric
                    .directory
                    .entries
                    .iter()
                    .all(|entry| entry.endpoint_id != default.endpoint_id)
            {
                return Err(NetworkPlanError::InvalidFabricPlan);
            }
        }
        let mut public_ids = BTreeSet::new();
        let mut public_addresses = BTreeSet::new();
        let mut public_endpoints = BTreeSet::new();
        for binding in &fabric.public_bindings {
            if binding.id.is_nil()
                || binding.project_id.is_empty()
                || binding.generation == 0
                || binding.public_address.is_unspecified()
                || !public_ids.insert(binding.id)
                || !public_addresses.insert(binding.public_address)
                || !public_endpoints.insert(binding.endpoint_id)
                || !fabric
                    .directory
                    .location(binding.endpoint_id)
                    .is_some_and(|endpoint| endpoint.project_id == binding.project_id)
            {
                return Err(NetworkPlanError::InvalidFabricPlan);
            }
        }
        let mut route_destinations = BTreeSet::new();
        let mut route_endpoints = BTreeSet::new();
        for route in &fabric.routes {
            if route.destination.prefix_len != 32
                || route.realm_id != fabric.realm_id
                || route.target_host.is_empty()
                || route.target_fabric_transport_ip.is_unspecified()
                || route.target_fabric_transport_ip.is_loopback()
                || route.endpoint_generation == 0
                || route.placement_generation == 0
                || route.realm_binding_generation != fabric.encapsulation.binding_generation
                || route.fabric_generation == 0
                || !route_destinations.insert(route.destination)
                || !route_endpoints.insert(route.endpoint_id)
                || fabric
                    .directory
                    .location(route.endpoint_id)
                    .is_none_or(|entry| {
                        entry.fixed_ip != route.destination.network
                            || entry.selected_host != route.target_host
                    })
            {
                return Err(NetworkPlanError::InvalidFabricPlan);
            }
        }
        let mut peer_hosts = BTreeSet::new();
        let mut peer_transport_ips = BTreeSet::new();
        for peer in &fabric.peers {
            if peer.host_id.is_empty()
                || peer.host_id == fabric.local_host
                || peer.public_key.is_empty()
                || peer.underlay_endpoint.is_empty()
                || peer.fabric_transport_ip.is_unspecified()
                || peer.fabric_transport_ip.is_loopback()
                || peer.fabric_generation == 0
                || !peer_hosts.insert(peer.host_id.as_str())
                || !peer_transport_ips.insert(peer.fabric_transport_ip)
            {
                return Err(NetworkPlanError::InvalidFabricPlan);
            }
            if !fabric.routes.iter().any(|route| {
                route.target_host == peer.host_id
                    && route.target_fabric_transport_ip == peer.fabric_transport_ip
            }) {
                return Err(NetworkPlanError::InvalidFabricPlan);
            }
        }
        if fabric.routes.iter().any(|route| {
            !peer_hosts.contains(route.target_host.as_str())
                || !fabric.peers.iter().any(|peer| {
                    peer.host_id == route.target_host
                        && peer.fabric_transport_ip == route.target_fabric_transport_ip
                })
        }) {
            return Err(NetworkPlanError::InvalidFabricPlan);
        }
        Ok(())
    }
}

/// Builds a node plan whose only execution unit is one complete canonical L3
/// gateway snapshot. This is used for gateway lifecycle operations that have
/// no endpoint plan to carry the gateway, such as deleting an unattached
/// gateway or detaching a Realm with no ports.
pub fn compile_l3_gateway_network_plan(
    gateway: o3k_domain::L3GatewayExecutionPlan,
    node_id: &str,
    operation_id: Uuid,
    deadline_unix_ms: u64,
) -> Result<NodeNetworkPlan, NetworkPlanError> {
    if node_id.trim().is_empty() {
        return Err(NetworkPlanError::InvalidGatewayPlan);
    }
    let mut plan = NodeNetworkPlan {
        schema_version: NODE_NETWORK_PLAN_SCHEMA_VERSION,
        plan_id: gateway.gateway_id,
        node_id: node_id.to_owned(),
        operation_id,
        deadline_unix_ms,
        resource_generations: BTreeMap::from([(gateway.gateway_id, gateway.gateway_generation)]),
        intents: Vec::new(),
        fabric: None,
        gateway: Some(gateway),
        fingerprint_sha256: String::new(),
    };
    plan.fingerprint_sha256 = canonical_plan_fingerprint(&plan)?;
    Ok(plan)
}

/// Compile the currently supported flat attachment projection into the same
/// canonical per-node plan used by routed providers. This helper is kept in
/// the network application boundary so callers cannot construct a wire-only
/// payload that bypasses plan validation.
pub struct AttachmentPlanInput<'a> {
    pub endpoint_id: Uuid,
    pub realm_id: Uuid,
    pub project_id: &'a str,
    pub mac: &'a str,
    pub fixed_ip: std::net::Ipv4Addr,
    pub subnet_cidr: &'a str,
    pub node_id: &'a str,
    pub operation_id: Uuid,
    pub deadline_unix_ms: u64,
    pub public_address: Option<std::net::Ipv4Addr>,
    pub external_realm_id: Option<Uuid>,
    pub policies: Vec<PolicyIntent>,
}

pub fn compile_attachment_plan(
    input: AttachmentPlanInput<'_>,
) -> Result<NodeNetworkPlan, NetworkPlanError> {
    compile_attachment_plan_with_defaults(input, Vec::new())
}

pub fn compile_attachment_plan_with_defaults(
    input: AttachmentPlanInput<'_>,
    policy_defaults: Vec<PolicyDefaultIntent>,
) -> Result<NodeNetworkPlan, NetworkPlanError> {
    let AttachmentPlanInput {
        endpoint_id,
        realm_id,
        project_id,
        mac,
        fixed_ip,
        subnet_cidr,
        node_id,
        operation_id,
        deadline_unix_ms,
        public_address,
        external_realm_id,
        policies,
    } = input;
    let has_policies = !policies.is_empty() || !policy_defaults.is_empty();
    let (network, prefix_len) = subnet_cidr
        .split_once('/')
        .ok_or(NetworkPlanError::InvalidPrefix)?;
    let network = network
        .parse()
        .map_err(|_| NetworkPlanError::InvalidPrefix)?;
    let prefix_len = prefix_len
        .parse::<u8>()
        .map_err(|_| NetworkPlanError::InvalidPrefix)?;
    let prefix =
        o3k_domain::Ipv4Prefix::new(network, prefix_len).ok_or(NetworkPlanError::InvalidPrefix)?;
    let intent = NetworkIntent {
        id: endpoint_id,
        generation: 1,
        project_id: project_id.to_owned(),
        realm: AddressRealm {
            id: realm_id,
            network_id: endpoint_id,
            project_id: project_id.to_owned(),
            prefix,
            overlapping_prefixes: false,
        },
        address_pools: Vec::new(),
        endpoints: vec![o3k_domain::EndpointIntent {
            id: endpoint_id,
            project_id: project_id.to_owned(),
            realm_id,
            mac: mac.to_owned(),
            fixed_ip,
            generation: 1,
        }],
        routes: Vec::new(),
        gateways: Vec::new(),
        egress: external_realm_id
            .map(|external_realm_id| {
                vec![o3k_domain::EgressIntent {
                    external_realm_id,
                    enabled: true,
                    nat: true,
                }]
            })
            .unwrap_or_default(),
        public_addresses: public_address
            .map(|public_address| {
                vec![o3k_domain::PublicAddressBindingIntent {
                    id: endpoint_id,
                    project_id: project_id.to_owned(),
                    public_address,
                    endpoint_id,
                    generation: 1,
                }]
            })
            .unwrap_or_default(),
        policies,
        state: o3k_domain::NetworkIntentState::Requested,
    };
    let mut capabilities: HashSet<NetworkCapability> = [
        NetworkCapability::Ipv4,
        NetworkCapability::EndpointAttachment,
    ]
    .into_iter()
    .collect();
    if public_address.is_some() {
        capabilities.insert(NetworkCapability::PublicAddressRealization);
    }
    if external_realm_id.is_some() {
        capabilities.insert(NetworkCapability::Routing);
        capabilities.insert(NetworkCapability::Nat);
    }
    if has_policies {
        capabilities.insert(NetworkCapability::StatefulPolicy);
    }
    let mut plan = compile_node_network_plan(
        &intent,
        node_id,
        operation_id,
        deadline_unix_ms,
        &capabilities,
        &[],
    )?;
    for default in policy_defaults {
        if default.endpoint_id != endpoint_id
            || default.policy_id.is_nil()
            || default.generation == 0
            || default.stateful_mode != PolicyStatefulMode::Stateful
        {
            return Err(NetworkPlanError::InvalidPolicy);
        }
        plan.resource_generations
            .insert(default.policy_id, default.generation);
        plan.intents.push(NetworkPlanIntent::PolicyDefault(default));
    }
    plan.intents
        .sort_by_key(|intent| serde_json::to_string(intent).unwrap_or_default());
    plan.fingerprint_sha256 = canonical_plan_fingerprint(&plan)?;
    Ok(plan)
}

/// Adds routing derived from the canonical L3Gateway graph to a complete
/// endpoint plan. The mutation is applied to the derived plan only; gateway
/// records remain the source of truth and the existing attachment-plan API
/// remains compatible for callers that have no gateway.
pub fn add_l3_gateway_routing(
    mut plan: NodeNetworkPlan,
    routes: Vec<o3k_domain::GatewayIntent>,
    egress: Vec<o3k_domain::EgressIntent>,
) -> Result<NodeNetworkPlan, NetworkPlanError> {
    if routes.is_empty() && egress.is_empty() {
        return Ok(plan);
    }
    plan.intents
        .extend(routes.into_iter().map(NetworkPlanIntent::Gateway));
    plan.intents
        .extend(egress.into_iter().map(NetworkPlanIntent::Egress));
    plan.intents
        .sort_by_key(|value| serde_json::to_string(value).unwrap_or_default());
    plan.fingerprint_sha256 = canonical_plan_fingerprint(&plan)?;
    Ok(plan)
}

/// Compiles one canonical intent into a stable semantic node plan. The
/// `realms` slice represents existing routed realms in the selected profile;
/// P9 rejects overlap before any provider mutation.
pub fn compile_node_network_plan(
    intent: &NetworkIntent,
    node_id: &str,
    operation_id: Uuid,
    deadline_unix_ms: u64,
    capabilities: &HashSet<NetworkCapability>,
    realms: &[AddressRealm],
) -> Result<NodeNetworkPlan, NetworkPlanError> {
    if node_id.is_empty() || intent.realm.project_id != intent.project_id {
        return Err(NetworkPlanError::OwnershipViolation);
    }
    let overlaps_existing_realm = realms
        .iter()
        .any(|realm| realm.id != intent.realm.id && realm.prefix.overlaps(intent.realm.prefix));
    if overlaps_existing_realm
        && (!intent.realm.overlapping_prefixes
            || !capabilities.contains(&NetworkCapability::OverlappingAddressRealms)
            || !capabilities.contains(&NetworkCapability::EncapsulationModes))
    {
        return Err(NetworkPlanError::OverlappingRealm);
    }
    require_capability(capabilities, NetworkCapability::Ipv4)?;
    require_capability(capabilities, NetworkCapability::EndpointAttachment)?;
    if !intent.routes.is_empty() {
        require_capability(capabilities, NetworkCapability::Routing)?;
    }
    if !intent.gateways.is_empty() {
        require_capability(capabilities, NetworkCapability::Routing)?;
    }
    if intent.egress.iter().any(|egress| egress.enabled) {
        require_capability(capabilities, NetworkCapability::Routing)?;
        if intent.egress.iter().any(|egress| egress.nat) {
            require_capability(capabilities, NetworkCapability::Nat)?;
        }
    }
    if !intent.public_addresses.is_empty() {
        require_capability(capabilities, NetworkCapability::PublicAddressRealization)?;
    }
    if !intent.policies.is_empty() {
        require_capability(capabilities, NetworkCapability::StatefulPolicy)?;
    }

    let mut generations = BTreeMap::new();
    let mut endpoint_addresses = HashSet::new();
    let mut endpoint_macs = HashSet::new();
    let gateway = intent
        .address_pools
        .iter()
        .find_map(|pool| pool.gateway)
        .or_else(|| {
            u32::from(intent.realm.prefix.network)
                .checked_add(1)
                .map(Ipv4Addr::from)
        })
        .ok_or(NetworkPlanError::InvalidAddressPool)?;
    for pool in &intent.address_pools {
        if pool.project_id != intent.project_id
            || pool.realm_id != intent.realm.id
            || pool.prefix.prefix_len < intent.realm.prefix.prefix_len
            || !intent.realm.prefix.contains(pool.prefix.network)
            || !pool.prefix.contains(pool.first_usable)
            || !pool.prefix.contains(pool.last_usable)
            || pool.first_usable == pool.prefix.network
            || pool.last_usable == pool.prefix.network
            || broadcast_address(pool.prefix).is_some_and(|broadcast| {
                pool.first_usable == broadcast || pool.last_usable == broadcast
            })
            || u32::from(pool.first_usable) > u32::from(pool.last_usable)
            || pool.gateway.is_some_and(|gateway| {
                !pool.prefix.contains(gateway)
                    || gateway == pool.prefix.network
                    || broadcast_address(pool.prefix).is_some_and(|broadcast| gateway == broadcast)
                    || u32::from(pool.first_usable) <= u32::from(gateway)
                        && u32::from(gateway) <= u32::from(pool.last_usable)
            })
            || u32::from(pool.first_usable) <= u32::from(gateway)
                && u32::from(gateway) <= u32::from(pool.last_usable)
        {
            return Err(NetworkPlanError::InvalidAddressPool);
        }
    }
    let mut intents =
        Vec::with_capacity(intent.endpoints.len() + intent.routes.len() + intent.policies.len());
    intents.push(NetworkPlanIntent::AddressRealm {
        realm_id: intent.realm.id,
        prefix: intent.realm.prefix,
        gateway,
    });
    for endpoint in &intent.endpoints {
        if endpoint.project_id != intent.project_id
            || !intent.realm.prefix.contains(endpoint.fixed_ip)
            || endpoint.fixed_ip == intent.realm.prefix.network
            || broadcast_address(intent.realm.prefix)
                .is_some_and(|broadcast| endpoint.fixed_ip == broadcast)
        {
            return Err(NetworkPlanError::AddressOutsideRealm);
        }
        let canonical_mac = endpoint.mac.to_ascii_lowercase();
        if !valid_mac(&canonical_mac) {
            return Err(NetworkPlanError::ConflictingEndpoint);
        }
        if generations
            .insert(endpoint.id, endpoint.generation)
            .is_some()
            || !endpoint_addresses.insert(endpoint.fixed_ip)
            || !endpoint_macs.insert(canonical_mac.clone())
            || endpoint.fixed_ip == gateway
        {
            return Err(NetworkPlanError::ConflictingEndpoint);
        }
        intents.push(NetworkPlanIntent::EndpointAttachment {
            endpoint_id: endpoint.id,
            mac: canonical_mac,
            fixed_ip: endpoint.fixed_ip,
            generation: endpoint.generation,
        });
        intents.push(NetworkPlanIntent::AddressAssignment {
            endpoint_id: endpoint.id,
            address: endpoint.fixed_ip,
            generation: endpoint.generation,
        });
    }
    let endpoint_ids: HashSet<Uuid> = generations.keys().copied().collect();
    let mut public_addresses = HashSet::new();
    for binding in &intent.public_addresses {
        if binding.project_id != intent.project_id
            || !endpoint_ids.contains(&binding.endpoint_id)
            || !public_addresses.insert(binding.public_address)
        {
            return Err(NetworkPlanError::OwnershipViolation);
        }
    }
    for policy in &intent.policies {
        if !endpoint_ids.contains(&policy.endpoint_id)
            || policy.ports.is_some_and(|ports| ports.start > ports.end)
            || policy.ports.is_some_and(|_| {
                matches!(
                    policy.protocol,
                    NetworkProtocol::Any | NetworkProtocol::Icmp
                )
            })
            || (matches!(policy.direction, PolicyDirection::Ingress)
                && policy.destination.is_some())
            || (matches!(policy.direction, PolicyDirection::Egress) && policy.source.is_some())
        {
            return Err(NetworkPlanError::InvalidPolicy);
        }
    }
    for gateway in &intent.gateways {
        if !gateway.external && !intent.realm.prefix.contains(gateway.gateway) {
            return Err(NetworkPlanError::AddressOutsideRealm);
        }
    }
    intents.extend(intent.routes.iter().cloned().map(NetworkPlanIntent::Route));
    intents.extend(
        intent
            .gateways
            .iter()
            .cloned()
            .map(NetworkPlanIntent::Gateway),
    );
    intents.extend(intent.egress.iter().cloned().map(NetworkPlanIntent::Egress));
    intents.extend(
        intent
            .public_addresses
            .iter()
            .cloned()
            .map(NetworkPlanIntent::PublicAddressBinding),
    );
    intents.extend(
        intent
            .policies
            .iter()
            .cloned()
            .map(NetworkPlanIntent::Policy),
    );
    intents.sort_by_key(|value| serde_json::to_string(value).unwrap_or_default());

    let unsigned = (
        &intent.id,
        node_id,
        &operation_id,
        &NODE_NETWORK_PLAN_SCHEMA_VERSION,
        &generations,
        &intents,
    );
    let bytes = serde_json::to_vec(&unsigned).map_err(|_| NetworkPlanError::Serialization)?;
    use sha2::{Digest, Sha256};
    let fingerprint_sha256 = format!("{:x}", Sha256::digest(bytes));
    Ok(NodeNetworkPlan {
        schema_version: NODE_NETWORK_PLAN_SCHEMA_VERSION,
        plan_id: intent.id,
        node_id: node_id.to_owned(),
        operation_id,
        deadline_unix_ms,
        resource_generations: generations,
        intents,
        fabric: None,
        gateway: None,
        fingerprint_sha256,
    })
}

/// Accepts an equivalent replay and rejects a payload change for the same
/// plan identity before an execution provider can mutate anything.
pub fn validate_plan_replay(
    existing: &NodeNetworkPlan,
    candidate: &NodeNetworkPlan,
) -> Result<(), NetworkPlanError> {
    let same_identity = existing.plan_id == candidate.plan_id
        && existing.node_id == candidate.node_id
        && existing.operation_id == candidate.operation_id;
    if same_identity
        && (existing.schema_version != candidate.schema_version
            || existing.fingerprint_sha256 != candidate.fingerprint_sha256)
    {
        return Err(NetworkPlanError::ConflictingPlan);
    }
    Ok(())
}

/// Recomputes the transport fingerprint from the semantic plan fields. The
/// executor uses this at the trust boundary so a caller cannot mark an
/// arbitrary mutated payload with a syntactically valid but unrelated hash.
pub fn canonical_plan_fingerprint(plan: &NodeNetworkPlan) -> Result<String, NetworkPlanError> {
    let mut intents = plan.intents.clone();
    let mut keyed = Vec::with_capacity(intents.len());
    for intent in intents.drain(..) {
        let key = serde_json::to_vec(&intent).map_err(|_| NetworkPlanError::Serialization)?;
        keyed.push((key, intent));
    }
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    intents = keyed.into_iter().map(|(_, intent)| intent).collect();
    let bytes = if let Some(fabric) = &plan.fabric {
        if let Some(gateway) = &plan.gateway {
            serde_json::to_vec(&(
                &plan.plan_id,
                &plan.node_id,
                &plan.operation_id,
                &plan.schema_version,
                &plan.resource_generations,
                &intents,
                gateway,
                fabric,
            ))
        } else {
            serde_json::to_vec(&(
                &plan.plan_id,
                &plan.node_id,
                &plan.operation_id,
                &plan.schema_version,
                &plan.resource_generations,
                &intents,
                fabric,
            ))
        }
    } else if let Some(gateway) = &plan.gateway {
        serde_json::to_vec(&(
            &plan.plan_id,
            &plan.node_id,
            &plan.operation_id,
            &plan.schema_version,
            &plan.resource_generations,
            &intents,
            gateway,
        ))
    } else {
        serde_json::to_vec(&(
            &plan.plan_id,
            &plan.node_id,
            &plan.operation_id,
            &plan.schema_version,
            &plan.resource_generations,
            &intents,
        ))
    }
    .map_err(|_| NetworkPlanError::Serialization)?;
    use sha2::{Digest, Sha256};
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn broadcast_address(prefix: o3k_domain::Ipv4Prefix) -> Option<Ipv4Addr> {
    let host_bits = 32u32.saturating_sub(u32::from(prefix.prefix_len));
    let size = 1u64.checked_shl(host_bits)?;
    let value = u64::from(u32::from(prefix.network)) + size - 1;
    u32::try_from(value).ok().map(Ipv4Addr::from)
}

fn valid_mac(value: &str) -> bool {
    value.len() == 17
        && value.split(':').count() == 6
        && value
            .split(':')
            .all(|part| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn require_capability(
    capabilities: &HashSet<NetworkCapability>,
    capability: NetworkCapability,
) -> Result<(), NetworkPlanError> {
    capabilities
        .contains(&capability)
        .then_some(())
        .ok_or(NetworkPlanError::UnsupportedCapability(capability))
}

fn validate_policy_shape(policy: &PolicyIntent) -> Result<(), NetworkError> {
    if policy.id.is_nil()
        || policy.ports.is_some_and(|ports| ports.start > ports.end)
        || policy.ports.is_some_and(|_| {
            matches!(
                policy.protocol,
                NetworkProtocol::Any | NetworkProtocol::Icmp
            )
        })
        || (matches!(policy.direction, PolicyDirection::Ingress) && policy.destination.is_some())
        || (matches!(policy.direction, PolicyDirection::Egress) && policy.source.is_some())
    {
        return Err(NetworkError::InvalidRequest);
    }
    Ok(())
}

fn canonical_policy_record(
    project_id: &str,
    policy: &PolicyIntent,
) -> o3k_store::CanonicalNetworkPolicyRecord {
    let prefix = |value: Option<Ipv4Prefix>| {
        value.map(|prefix| format!("{}/{}", prefix.network, prefix.prefix_len))
    };
    o3k_store::CanonicalNetworkPolicyRecord {
        id: policy.id,
        project_id: project_id.to_owned(),
        endpoint_id: policy.endpoint_id,
        direction: format!("{:?}", policy.direction),
        protocol: format!("{:?}", policy.protocol),
        port_min: policy.ports.map(|ports| ports.start),
        port_max: policy.ports.map(|ports| ports.end),
        source: prefix(policy.source),
        destination: prefix(policy.destination),
        action: format!("{:?}", policy.action),
        generation: 1,
        state: "active".to_owned(),
    }
}

fn security_group_from_policy(
    policy: o3k_store::CanonicalReusableNetworkPolicyRecord,
) -> o3k_store::SecurityGroupRecord {
    o3k_store::SecurityGroupRecord {
        id: policy.id,
        project_id: policy.project_id,
        name: policy.name,
        description: policy.description,
    }
}

fn security_group_rule_from_policy(
    rule: o3k_store::CanonicalNetworkPolicyRuleRecord,
) -> o3k_store::SecurityGroupRuleRecord {
    o3k_store::SecurityGroupRuleRecord {
        id: rule.id,
        security_group_id: rule.policy_id,
        project_id: rule.project_id,
        direction: rule.direction.to_lowercase(),
        protocol: rule.protocol.to_lowercase(),
        port_min: rule.port_min,
        port_max: rule.port_max,
        remote_ip_prefix: rule.remote_selector,
    }
}

fn policy_from_canonical_record(
    record: o3k_store::CanonicalNetworkPolicyRecord,
) -> Result<PolicyIntent, NetworkError> {
    let parse_prefix = |value: Option<String>| {
        value
            .as_deref()
            .map(parse_security_group_prefix)
            .transpose()
    };
    let policy = PolicyIntent {
        id: record.id,
        endpoint_id: record.endpoint_id,
        direction: parse_security_group_direction(&record.direction)?,
        protocol: parse_security_group_protocol(&record.protocol)?,
        ports: record
            .port_min
            .zip(record.port_max)
            .map(|(start, end)| PortRange { start, end }),
        source: parse_prefix(record.source)?,
        destination: parse_prefix(record.destination)?,
        action: match record.action.as_str() {
            "Allow" | "allow" => PolicyAction::Allow,
            "Deny" | "deny" => PolicyAction::Deny,
            _ => return Err(NetworkError::InvalidRequest),
        },
    };
    validate_policy_shape(&policy)?;
    Ok(policy)
}

/// Canonical binding state of a port on its selected host.
///
/// The durable store persists the string projections (persistence
/// projection); this service is the only authority that transitions between
/// states. `None` in the store means no host was ever selected and no
/// observation exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortBindingState {
    /// A create dispatch selected a host but realization is not yet observed.
    Binding,
    /// The host observed the binding as realized.
    Bound,
    /// The host observed the binding as not realized.
    Down,
    /// The host observed a terminal failure.
    Error,
}

impl PortBindingState {
    /// The durable string projection.
    pub fn as_str(self) -> &'static str {
        match self {
            PortBindingState::Binding => "binding",
            PortBindingState::Bound => "bound",
            PortBindingState::Down => "down",
            PortBindingState::Error => "error",
        }
    }

    /// Parses the durable string projection. Unknown values are rejected so
    /// free-form state can never be persisted through the service.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "binding" => Some(PortBindingState::Binding),
            "bound" => Some(PortBindingState::Bound),
            "down" => Some(PortBindingState::Down),
            "error" => Some(PortBindingState::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("network resource not found")]
    NotFound,
    #[error("network resource already exists or is still in use")]
    Conflict,
    #[error("network request is invalid")]
    InvalidRequest,
    #[error("quota exceeded for {key}: limit {limit}, used {used}, requested {requested}")]
    QuotaExceeded {
        key: LimitKey,
        limit: LimitValue,
        used: u64,
        requested: u64,
    },
    #[error("subnet allocation pool is exhausted")]
    PoolExhausted,
    #[error("network store error")]
    Store(#[source] o3k_store::StoreError),
    #[error("network metadata is corrupt")]
    CorruptMetadata(#[source] serde_json::Error),
}

fn map_store_error(error: o3k_store::StoreError) -> NetworkError {
    match error {
        o3k_store::StoreError::ResourceAlreadyExists => NetworkError::Conflict,
        o3k_store::StoreError::NetworkNotFound | o3k_store::StoreError::ResourceNotFound => {
            NetworkError::NotFound
        }
        o3k_store::StoreError::NetworkInUse => NetworkError::Conflict,
        o3k_store::StoreError::OwnershipConflict => NetworkError::InvalidRequest,
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
        other => NetworkError::Store(other),
    }
}

fn realm_delete_operation(
    project_id: &str,
    realm_id: Uuid,
) -> Result<
    (
        o3k_store::OperationRecord,
        o3k_store::CanonicalOperationRecord,
        o3k_store::IdempotencyReservationRequest,
    ),
    NetworkError,
> {
    let action =
        ActionId::new("network", "DeleteRealm").map_err(|_| NetworkError::InvalidRequest)?;
    let resource_type =
        ResourceType::new("network", "address_realm").map_err(|_| NetworkError::InvalidRequest)?;
    let operation_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("o3k:network:realm-delete:{project_id}:{realm_id}").as_bytes(),
    );
    let scope = OwnershipScope::project(ScopeId::new_unchecked(project_id.to_owned()), None, None);
    let kernel = o3k_kernel::Operation::new(
        operation_id,
        "network",
        action.clone(),
        "o3k:network-service",
        scope,
        resource_type.clone(),
        Some(ResourceId::new_unchecked(realm_id.to_string())),
        None,
    );
    let canonical = o3k_store::CanonicalOperationRecord::from_kernel_operation(&kernel)
        .map_err(map_store_error)?;
    let operation = o3k_store::OperationRecord {
        id: operation_id,
        resource_id: realm_id,
        kind: "lifecycle:realm-delete".to_owned(),
        state: o3k_store::OperationState::Pending,
        provider_operation_id: None,
        error_category: None,
        error_message: None,
    };
    let request = o3k_store::IdempotencyReservationRequest::from_semantics(
        project_id,
        action.to_string(),
        format!("canonical:realm-delete:{realm_id}"),
        &resource_type.to_string(),
        Some(&realm_id.to_string()),
        &serde_json::json!({"realm_id": realm_id}),
        operation_id,
    )
    .map_err(map_store_error)?;
    Ok((operation, canonical, request))
}

fn canonical_network_projection(network: o3k_store::CanonicalNetworkRecord) -> NetworkRecord {
    NetworkRecord {
        id: network.id,
        name: network.name,
        project_id: network.project_id,
        status: network.state.to_ascii_uppercase(),
    }
}

#[derive(Clone)]
pub struct NetworkService {
    inner: Arc<Inner>,
    lock: Arc<tokio::sync::Mutex<()>>,
    authorizer: Arc<dyn Authorizer>,
    audit_sink: Arc<dyn AuditSink>,
}

struct Inner {
    root: PathBuf,
    repository: Arc<dyn o3k_store::NetworkRepository>,
}

/// Canonical network reconstruction result.  Compatibility projections and
/// provider plans are derived from this durable graph; they are never used to
/// recover missing canonical children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalNetworkSnapshot {
    pub network: o3k_store::CanonicalNetworkRecord,
    pub realms: Vec<o3k_store::CanonicalAddressRealmRecord>,
    pub pools: BTreeMap<Uuid, Vec<o3k_store::CanonicalAddressPoolRecord>>,
    pub endpoints: BTreeMap<Uuid, Vec<o3k_store::CanonicalEndpointRecord>>,
    /// Canonical L3 gateway authority relevant to this network's realms.
    /// Provider plans are derived from this graph; it is not compatibility
    /// state and does not redefine AddressRealm identity.
    pub l3_gateways: Vec<(
        o3k_store::CanonicalL3GatewayRecord,
        Vec<o3k_store::CanonicalL3GatewayAttachmentRecord>,
    )>,
}

/// Compiles the canonical gateway graph into the existing provider-neutral
/// routing intents. AddressRealm remains the unit of address interpretation;
/// this function only derives connectivity from the gateway attachments.
pub type GatewayIntentMap = BTreeMap<
    Uuid,
    (
        Vec<o3k_domain::GatewayIntent>,
        Vec<o3k_domain::EgressIntent>,
    ),
>;

pub fn compile_l3_gateway_intents(
    gateway: &o3k_store::CanonicalL3GatewayRecord,
    attachments: &[o3k_store::CanonicalL3GatewayAttachmentRecord],
    realms: &[o3k_store::CanonicalAddressRealmRecord],
    pools: &BTreeMap<Uuid, Vec<o3k_store::CanonicalAddressPoolRecord>>,
) -> Result<GatewayIntentMap, NetworkError> {
    if gateway.state != "active" || gateway.generation == 0 {
        return Err(NetworkError::InvalidRequest);
    }
    let mut realm_map = BTreeMap::new();
    for realm in realms {
        if realm.project_id != gateway.project_id || realm.state != "active" {
            continue;
        }
        let (network, prefix) = realm
            .prefix
            .split_once('/')
            .ok_or(NetworkError::InvalidRequest)?;
        let address = network.parse().map_err(|_| NetworkError::InvalidRequest)?;
        let prefix_len = prefix.parse().map_err(|_| NetworkError::InvalidRequest)?;
        let prefix = Ipv4Prefix::new(address, prefix_len).ok_or(NetworkError::InvalidRequest)?;
        realm_map.insert(realm.id, prefix);
    }
    let attached: BTreeSet<Uuid> = attachments
        .iter()
        .filter(|attachment| {
            attachment.project_id == gateway.project_id && attachment.state == "active"
        })
        .map(|attachment| attachment.realm_id)
        .collect();
    let mut result = BTreeMap::new();
    for realm_id in &attached {
        let local = realm_map.get(realm_id).ok_or(NetworkError::NotFound)?;
        let local_gateway = pools
            .get(realm_id)
            .and_then(|items| items.iter().find_map(|pool| pool.gateway))
            .or_else(|| u32::from(local.network).checked_add(1).map(Ipv4Addr::from))
            .ok_or(NetworkError::InvalidRequest)?;
        let mut routes = Vec::new();
        for remote_id in &attached {
            if remote_id != realm_id {
                routes.push(o3k_domain::GatewayIntent {
                    destination: *realm_map.get(remote_id).ok_or(NetworkError::NotFound)?,
                    gateway: local_gateway,
                    external: false,
                });
            }
        }
        let egress = gateway
            .external_realm_id
            .map(|external_realm_id| {
                vec![o3k_domain::EgressIntent {
                    external_realm_id,
                    enabled: true,
                    nat: gateway.enable_snat,
                }]
            })
            .unwrap_or_default();
        result.insert(*realm_id, (routes, egress));
    }
    Ok(result)
}

/// Result of observing one provider-owned Realm cleanup identity.  A Realm
/// remains canonical while the provider outcome is present or unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealmCleanupObservation {
    Absent(o3k_store::CanonicalRealmBindingRecord),
    Present(o3k_store::CanonicalRealmBindingRecord),
    Unknown {
        binding: o3k_store::CanonicalRealmBindingRecord,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealmCleanupProgress {
    Deleting { operation_id: Uuid, generation: u64 },
    AwaitingObservation { operation_id: Uuid, generation: u64 },
    Removed { operation_id: Uuid },
}

impl NetworkService {
    /// Creates the provider-independent L3 gateway authority. This is
    /// intentionally persistence-only; Neutron projection and provider
    /// realization are layered above the canonical graph.
    pub async fn create_l3_gateway_for_project(
        &self,
        project_id: &str,
        name: String,
        external_realm_id: Option<Uuid>,
        enable_snat: bool,
    ) -> Result<o3k_store::CanonicalL3GatewayRecord, NetworkError> {
        if name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        if let Some(realm_id) = external_realm_id
            && self
                .inner
                .repository
                .get_canonical_realm(project_id, &realm_id)
                .await
                .map_err(map_store_error)?
                .is_none()
        {
            return Err(NetworkError::NotFound);
        }
        let gateway = o3k_store::CanonicalL3GatewayRecord {
            id: Uuid::now_v7(),
            project_id: project_id.to_owned(),
            name,
            external_realm_id,
            enable_snat,
            generation: 1,
            state: "active".to_owned(),
        };
        self.inner
            .repository
            .insert_canonical_l3_gateway(&gateway)
            .await
            .map_err(map_store_error)?;
        Ok(gateway)
    }

    pub async fn list_l3_gateways_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<o3k_store::CanonicalL3GatewayRecord>, NetworkError> {
        self.inner
            .repository
            .list_canonical_l3_gateways(project_id)
            .await
            .map_err(map_store_error)
    }

    pub async fn get_l3_gateway_for_project(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
    ) -> Result<o3k_store::CanonicalL3GatewayRecord, NetworkError> {
        self.inner
            .repository
            .get_canonical_l3_gateway(project_id, gateway_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)
    }

    pub async fn attach_l3_gateway_realm(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
        realm_id: &Uuid,
    ) -> Result<o3k_store::CanonicalL3GatewayAttachmentRecord, NetworkError> {
        let gateway = self
            .get_l3_gateway_for_project(project_id, gateway_id)
            .await?;
        if gateway.state != "active" {
            return Err(NetworkError::Conflict);
        }
        if self
            .inner
            .repository
            .get_canonical_realm(project_id, realm_id)
            .await
            .map_err(map_store_error)?
            .is_none()
        {
            return Err(NetworkError::NotFound);
        }
        if self
            .inner
            .repository
            .list_canonical_l3_gateway_attachments(project_id, gateway_id)
            .await
            .map_err(map_store_error)?
            .iter()
            .any(|attachment| attachment.realm_id == *realm_id)
        {
            // The relation is a durable deletion reservation as well as a
            // compatibility object. Do not replace it while the provider is
            // still converging the detach.
            return Err(NetworkError::Conflict);
        }
        let attachment = o3k_store::CanonicalL3GatewayAttachmentRecord {
            id: Uuid::now_v7(),
            gateway_id: *gateway_id,
            realm_id: *realm_id,
            project_id: project_id.to_owned(),
            generation: 1,
            state: "active".to_owned(),
        };
        self.inner
            .repository
            .insert_canonical_l3_gateway_attachment(&attachment)
            .await
            .map_err(map_store_error)?;
        Ok(attachment)
    }

    pub async fn update_l3_gateway_for_project(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
        expected_generation: u64,
        name: String,
        external_realm_id: Option<Uuid>,
        enable_snat: bool,
    ) -> Result<o3k_store::CanonicalL3GatewayRecord, NetworkError> {
        if name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        if let Some(realm_id) = external_realm_id
            && self
                .inner
                .repository
                .get_canonical_realm(project_id, &realm_id)
                .await
                .map_err(map_store_error)?
                .is_none()
        {
            return Err(NetworkError::NotFound);
        }
        self.inner
            .repository
            .update_canonical_l3_gateway(
                project_id,
                gateway_id,
                expected_generation,
                &name,
                external_realm_id,
                enable_snat,
            )
            .await
            .map_err(map_store_error)
    }

    pub async fn delete_l3_gateway_for_project(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
        expected_generation: u64,
    ) -> Result<o3k_store::CanonicalL3GatewayRecord, NetworkError> {
        self.inner
            .repository
            .begin_canonical_l3_gateway_deletion(project_id, gateway_id, expected_generation)
            .await
            .map_err(map_store_error)
    }

    /// Finalizes a gateway deletion only after the provider has withdrawn the
    /// complete gateway realization and absence has been observed.  Keeping
    /// this separate from reservation makes the deleting row restart-safe.
    pub async fn finalize_l3_gateway_deletion_for_project(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
        expected_generation: u64,
    ) -> Result<(), NetworkError> {
        self.inner
            .repository
            .finalize_canonical_l3_gateway_deletion(project_id, gateway_id, expected_generation)
            .await
            .map_err(map_store_error)
    }

    pub async fn detach_l3_gateway_realm(
        &self,
        project_id: &str,
        attachment_id: &Uuid,
        expected_generation: u64,
    ) -> Result<o3k_store::CanonicalL3GatewayAttachmentRecord, NetworkError> {
        self.inner
            .repository
            .begin_canonical_l3_gateway_attachment_deletion(
                project_id,
                attachment_id,
                expected_generation,
            )
            .await
            .map_err(map_store_error)
    }

    /// Finalizes an attachment deletion only after the complete remaining
    /// gateway snapshot has converged and the detached provider link is absent.
    pub async fn finalize_l3_gateway_realm_detachment_for_project(
        &self,
        project_id: &str,
        attachment_id: &Uuid,
        expected_generation: u64,
    ) -> Result<(), NetworkError> {
        self.inner
            .repository
            .finalize_canonical_l3_gateway_attachment_deletion(
                project_id,
                attachment_id,
                expected_generation,
            )
            .await
            .map_err(map_store_error)
    }

    pub async fn list_l3_gateway_attachments(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
    ) -> Result<Vec<o3k_store::CanonicalL3GatewayAttachmentRecord>, NetworkError> {
        self.inner
            .repository
            .list_canonical_l3_gateway_attachments(project_id, gateway_id)
            .await
            .map_err(map_store_error)
    }

    /// Reconstructs the provider-independent multi-Realm gateway execution
    /// unit from canonical persistence. Linux namespace/interface details are
    /// intentionally supplied by the provider's derived Realm directory,
    /// never persisted in this plan.
    pub async fn compile_l3_gateway_execution_plan_for_project(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
    ) -> Result<o3k_domain::L3GatewayExecutionPlan, NetworkError> {
        let gateway = self
            .get_l3_gateway_for_project(project_id, gateway_id)
            .await?;
        let mut compilable_gateway = gateway.clone();
        if !matches!(gateway.state.as_str(), "active" | "deleting") {
            return Err(NetworkError::Conflict);
        }
        // The provider-neutral compiler validates an active desired snapshot.
        // A deleting row is nevertheless a valid durable removal reservation:
        // compile that snapshot without changing its persisted generation.
        compilable_gateway.state = "active".to_owned();
        let attachments = self
            .list_l3_gateway_attachments(project_id, gateway_id)
            .await?;
        let mut realms = BTreeMap::new();
        for attachment in &attachments {
            if let Some(realm) = self
                .inner
                .repository
                .get_canonical_realm(project_id, &attachment.realm_id)
                .await
                .map_err(map_store_error)?
            {
                realms.insert(realm.id, realm);
            }
        }
        if let Some(external_realm_id) = gateway.external_realm_id {
            let external = self
                .inner
                .repository
                .get_canonical_realm(project_id, &external_realm_id)
                .await
                .map_err(map_store_error)?
                .ok_or(NetworkError::NotFound)?;
            if external.state != "active" {
                return Err(NetworkError::Conflict);
            }
            realms.insert(external.id, external);
        }
        compile_l3_gateway_execution_plan(&compilable_gateway, &attachments, &realms)
            .map_err(|_| NetworkError::InvalidRequest)
    }

    pub async fn open(
        root: impl Into<PathBuf>,
        repository: Arc<dyn o3k_store::NetworkRepository>,
    ) -> Result<Self, NetworkError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|source| {
            NetworkError::Store(o3k_store::StoreError::CreateDataDirectory {
                path: root.clone(),
                source,
            })
        })?;
        let inner = Arc::new(Inner { root, repository });
        if inner.root.join("metadata.json").exists() {
            import_legacy_metadata(&inner.root, inner.repository.as_ref()).await?;
        }
        inner
            .repository
            .backfill_canonical_network_state()
            .await
            .map_err(map_store_error)?;
        let service = Self {
            inner,
            lock: Arc::new(tokio::sync::Mutex::new(())),
            authorizer: Arc::new(StaticAuthorizer::standard()),
            audit_sink: Arc::new(NoopAuditSink),
        };
        service.recover_realm_deletion_operations().await?;
        Ok(service)
    }

    /// Rebuilds one endpoint's effective policy from canonical reusable policy
    /// state. This is the runtime integration seam; the returned snapshot is
    /// still derived execution input and is never written back as policy
    /// authority.
    pub async fn compile_canonical_policy_for_endpoint(
        &self,
        project_id: &str,
        endpoint_id: Uuid,
    ) -> Result<(Vec<NetworkPlanIntent>, String), CanonicalPolicyServiceError> {
        CanonicalPolicyService::new(self.inner.repository.clone())
            .compile_endpoint(project_id, endpoint_id)
            .await
    }

    pub async fn affected_endpoints_for_canonical_policy(
        &self,
        project_id: &str,
        policy_id: Uuid,
    ) -> Result<Vec<Uuid>, CanonicalPolicyServiceError> {
        CanonicalPolicyService::new(self.inner.repository.clone())
            .affected_endpoints_for_policy(project_id, policy_id)
            .await
    }

    pub async fn reconcile_canonical_policy_for_endpoint<P>(
        &self,
        project_id: &str,
        endpoint_id: Uuid,
        expected_fingerprint: Option<&str>,
        provider: &P,
    ) -> Result<PolicyApplyOutcome, CanonicalPolicyServiceError>
    where
        P: PolicySnapshotRealizer,
    {
        CanonicalPolicyService::new(self.inner.repository.clone())
            .reconcile_endpoint_policy(project_id, endpoint_id, expected_fingerprint, provider)
            .await
    }

    pub async fn reconcile_canonical_policy_endpoints<P>(
        &self,
        project_id: &str,
        policy_id: Uuid,
        provider: &P,
    ) -> Result<Vec<(Uuid, PolicyApplyOutcome)>, CanonicalPolicyServiceError>
    where
        P: PolicySnapshotRealizer,
    {
        CanonicalPolicyService::new(self.inner.repository.clone())
            .reconcile_policy_endpoints(project_id, policy_id, provider)
            .await
    }

    pub async fn recover_canonical_policy_realizations<P>(
        &self,
        project_id: &str,
        provider: &P,
    ) -> Result<Vec<(Uuid, PolicyApplyOutcome)>, CanonicalPolicyServiceError>
    where
        P: PolicySnapshotRealizer,
    {
        CanonicalPolicyService::new(self.inner.repository.clone())
            .recover_policy_realizations(project_id, provider)
            .await
    }

    #[must_use]
    pub fn with_authorizer(mut self, authorizer: Arc<dyn Authorizer>) -> Self {
        self.authorizer = authorizer;
        self
    }

    #[must_use]
    pub fn with_audit_sink(mut self, audit_sink: Arc<dyn AuditSink>) -> Self {
        self.audit_sink = audit_sink;
        self
    }

    async fn authorize_canonical_action(
        &self,
        auth: &AuthContext,
        action_name: &str,
        resource_name: &str,
        resource_id: Option<Uuid>,
        parent: Option<(&str, Uuid)>,
    ) -> Result<(ServiceNamespace, ActionId, ResourceType), NetworkError> {
        let namespace = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let action =
            ActionId::new("network", action_name).map_err(|_| NetworkError::InvalidRequest)?;
        let resource_type = ResourceType::new("network", resource_name)
            .map_err(|_| NetworkError::InvalidRequest)?;
        let owner_scope = match resource_id {
            Some(id) => self
                .inner
                .repository
                .get_canonical_owner(resource_name, &id)
                .await
                .map_err(map_store_error)?
                .ok_or(NetworkError::NotFound)?,
            None => match parent {
                Some((parent_name, parent_id)) => self
                    .inner
                    .repository
                    .get_canonical_owner(parent_name, &parent_id)
                    .await
                    .map_err(map_store_error)?
                    .ok_or(NetworkError::NotFound)?,
                None => auth.effective_scope().id().as_str().to_owned(),
            },
        };
        let owner_scope = ScopeId::new(owner_scope).map_err(|_| NetworkError::InvalidRequest)?;
        let target = match resource_id {
            Some(id) => ResourceTarget::instance(
                resource_type.clone(),
                ResourceId::new(id.to_string()).map_err(|_| NetworkError::InvalidRequest)?,
                Some(owner_scope.clone()),
            ),
            None => ResourceTarget::collection(resource_type.clone(), Some(owner_scope.clone())),
        };
        let decision = self.authorizer.authorize(&AuthorizationRequest {
            auth_context: auth,
            action: action.clone(),
            resource_target: target,
        });
        if !decision.is_allowed() {
            self.audit_sink.record(
                &AuditEvent::from_auth(
                    auth,
                    namespace.clone(),
                    action.clone(),
                    AuditOutcome::Denied,
                )
                .with_resource(
                    resource_type.clone(),
                    resource_id.and_then(|id| ResourceId::new(id.to_string()).ok()),
                    Some(OwnershipScope::project(owner_scope, None, None)),
                )
                .with_decision(decision.clone())
                .with_reason("unauthorized"),
            );
            return Err(match decision.reason() {
                DecisionReason::ScopeMismatch | DecisionReason::MissingOwnership => {
                    NetworkError::NotFound
                }
                _ => NetworkError::Unauthorized,
            });
        }
        Ok((namespace, action, resource_type))
    }

    fn audit_canonical_result(
        &self,
        auth: &AuthContext,
        namespace: ServiceNamespace,
        action: ActionId,
        resource_type: ResourceType,
        resource_id: Option<Uuid>,
        result: Result<(), &NetworkError>,
    ) {
        let outcome = if result.is_ok() {
            AuditOutcome::Succeeded
        } else {
            AuditOutcome::Failed
        };
        let mut event = AuditEvent::from_auth(auth, namespace, action, outcome).with_resource(
            resource_type,
            resource_id.and_then(|id| ResourceId::new(id.to_string()).ok()),
            Some(auth.effective_scope().clone()),
        );
        if let Err(error) = result {
            event = event.with_reason(error.to_string());
        }
        self.audit_sink.record(&event);
    }

    async fn recover_realm_deletion_operations(&self) -> Result<(), NetworkError> {
        let operations = self
            .inner
            .repository
            .list_non_terminal_lifecycle_operations()
            .await
            .map_err(map_store_error)?;
        for operation in operations {
            if operation.kind != "lifecycle:realm-delete" {
                continue;
            }
            let canonical = self
                .inner
                .repository
                .get_canonical_operation(operation.id)
                .await
                .map_err(map_store_error)?;
            let realm_id = canonical
                .resource_id
                .as_deref()
                .ok_or(NetworkError::InvalidRequest)?
                .parse::<Uuid>()
                .map_err(|_| NetworkError::InvalidRequest)?;
            let realm = self
                .inner
                .repository
                .get_canonical_realm(&canonical.owner_scope, &realm_id)
                .await
                .map_err(map_store_error)?;
            let Some(realm) = realm else {
                let update = o3k_store::CanonicalOperationLifecycleUpdate::new(
                    o3k_kernel::OperationState::Succeeded,
                    canonical.attempt.saturating_add(1),
                    canonical.started_at.clone(),
                    Some(
                        canonical
                            .started_at
                            .clone()
                            .unwrap_or_else(|| canonical.created_at.clone()),
                    ),
                    None,
                )
                .map_err(map_store_error)?;
                self.inner
                    .repository
                    .update_canonical_operation_lifecycle(operation.id, &update)
                    .await
                    .map_err(map_store_error)?;
                continue;
            };
            if realm.state == "active" {
                let deleting = match self
                    .inner
                    .repository
                    .begin_canonical_realm_deletion(
                        &canonical.owner_scope,
                        &realm_id,
                        realm.generation,
                    )
                    .await
                {
                    Ok(value) => value,
                    Err(o3k_store::StoreError::NetworkInUse) => {
                        let update = o3k_store::CanonicalOperationLifecycleUpdate::new(
                            o3k_kernel::OperationState::Retryable,
                            canonical.attempt.saturating_add(1),
                            canonical.started_at.clone(),
                            None,
                            Some("realm still has canonical dependents".to_owned()),
                        )
                        .map_err(map_store_error)?;
                        self.inner
                            .repository
                            .update_canonical_operation_lifecycle(operation.id, &update)
                            .await
                            .map_err(map_store_error)?;
                        continue;
                    }
                    Err(error) => return Err(map_store_error(error)),
                };
                self.inner
                    .repository
                    .update_resource(
                        realm_id,
                        i64::try_from(realm.generation)
                            .map_err(|_| NetworkError::InvalidRequest)?,
                        "deleting",
                        "deleting",
                        i64::try_from(deleting.generation)
                            .map_err(|_| NetworkError::InvalidRequest)?,
                        None,
                    )
                    .await
                    .map_err(map_store_error)?;
                let update = o3k_store::CanonicalOperationLifecycleUpdate::new(
                    o3k_kernel::OperationState::Running,
                    canonical.attempt.saturating_add(1),
                    Some(
                        canonical
                            .started_at
                            .clone()
                            .unwrap_or_else(|| canonical.created_at.clone()),
                    ),
                    None,
                    None,
                )
                .map_err(map_store_error)?;
                self.inner
                    .repository
                    .update_canonical_operation_lifecycle(operation.id, &update)
                    .await
                    .map_err(map_store_error)?;
                debug_assert_eq!(deleting.state, "deleting");
            }
        }
        Ok(())
    }

    /// Creates only the canonical Network row.  Address realms, pools and
    /// endpoints are independent child resources and are intentionally not
    /// synthesized here.
    pub async fn create_canonical_network_for_project(
        &self,
        project_id: &str,
        name: String,
    ) -> Result<o3k_store::CanonicalNetworkRecord, NetworkError> {
        if project_id.trim().is_empty() || name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock.lock().await;
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
        let network = o3k_store::CanonicalNetworkRecord {
            id: Uuid::now_v7(),
            project_id: project_id.to_owned(),
            name,
            admin_state_up: true,
            generation: 1,
            state: "active".to_owned(),
        };
        self.inner
            .repository
            .insert_canonical_network(&network)
            .await
            .map_err(map_store_error)?;
        Ok(network)
    }

    /// Authenticated entry point for canonical Network creation. The
    /// project-scoped primitive remains intentionally separate for internal
    /// reconciliation and migration callers.
    pub async fn create_canonical_network(
        &self,
        auth: &AuthContext,
        name: String,
    ) -> Result<o3k_store::CanonicalNetworkRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(auth, "CreateNetwork", "network", None, None)
            .await?;
        let result = self
            .create_canonical_network_for_project(auth.effective_scope().id().as_str(), name)
            .await;
        let audit_result = result.as_ref().map(|_| ());
        self.audit_canonical_result(auth, namespace, action, resource_type, None, audit_result);
        result
    }

    pub async fn get_canonical_network_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<o3k_store::CanonicalNetworkRecord, NetworkError> {
        self.inner
            .repository
            .get_canonical_network(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)
    }

    pub async fn get_canonical_network(
        &self,
        auth: &AuthContext,
        id: Uuid,
    ) -> Result<o3k_store::CanonicalNetworkRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(auth, "ReadNetwork", "network", Some(id), None)
            .await?;
        let result = self
            .get_canonical_network_for_project(auth.effective_scope().id().as_str(), id)
            .await;
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

    pub async fn list_canonical_networks_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<o3k_store::CanonicalNetworkRecord>, NetworkError> {
        self.inner
            .repository
            .list_canonical_networks(project_id)
            .await
            .map_err(map_store_error)
    }

    pub async fn list_canonical_networks(
        &self,
        auth: &AuthContext,
    ) -> Result<Vec<o3k_store::CanonicalNetworkRecord>, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(auth, "ListNetworks", "network", None, None)
            .await?;
        let result = self
            .list_canonical_networks_for_project(auth.effective_scope().id().as_str())
            .await;
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            None,
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn delete_canonical_network_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock.lock().await;
        self.inner
            .repository
            .delete_canonical_network(project_id, &id)
            .await
            .map_err(map_store_error)
    }

    pub async fn delete_canonical_network(
        &self,
        auth: &AuthContext,
        id: Uuid,
    ) -> Result<(), NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(auth, "DeleteNetwork", "network", Some(id), None)
            .await?;
        let result = self
            .delete_canonical_network_for_project(auth.effective_scope().id().as_str(), id)
            .await;
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

    pub async fn create_canonical_realm_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
        prefix: String,
        overlapping_prefixes: bool,
    ) -> Result<o3k_store::CanonicalAddressRealmRecord, NetworkError> {
        let prefix = Ipv4Net::parse(&prefix)?.canonical();
        let _guard = self.lock.lock().await;
        let network = self
            .inner
            .repository
            .get_canonical_network(project_id, &network_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        if network.state != "active" {
            return Err(NetworkError::Conflict);
        }
        let realm = o3k_store::CanonicalAddressRealmRecord {
            id: Uuid::now_v7(),
            network_id,
            project_id: project_id.to_owned(),
            prefix,
            overlapping_prefixes,
            generation: 1,
            state: "active".to_owned(),
        };
        self.inner
            .repository
            .insert_canonical_realm(&realm)
            .await
            .map_err(map_store_error)?;
        if let Err(error) = self
            .inner
            .repository
            .insert_resource(&o3k_store::ResourceRecord {
                id: realm.id,
                kind: "network:address_realm".to_owned(),
                project_id: realm.project_id.clone(),
                generation: 1,
                observed_generation: 1,
                desired_state: realm.state.clone(),
                observed_state: realm.state.clone(),
                provider_id: None,
            })
            .await
        {
            let _ = self
                .inner
                .repository
                .delete_canonical_realm(project_id, &realm.id)
                .await;
            return Err(map_store_error(error));
        }
        Ok(realm)
    }

    pub async fn create_canonical_realm(
        &self,
        auth: &AuthContext,
        network_id: Uuid,
        prefix: String,
        overlapping_prefixes: bool,
    ) -> Result<o3k_store::CanonicalAddressRealmRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "CreateAddressRealm",
                "address_realm",
                None,
                Some(("network", network_id)),
            )
            .await?;
        let result = self
            .create_canonical_realm_for_project(
                auth.effective_scope().id().as_str(),
                network_id,
                prefix,
                overlapping_prefixes,
            )
            .await;
        let audit_result = result.as_ref().map(|_| ());
        self.audit_canonical_result(auth, namespace, action, resource_type, None, audit_result);
        result
    }

    pub async fn list_canonical_realms_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
    ) -> Result<Vec<o3k_store::CanonicalAddressRealmRecord>, NetworkError> {
        if self
            .get_canonical_network_for_project(project_id, network_id)
            .await
            .is_err()
        {
            return Err(NetworkError::NotFound);
        }
        self.inner
            .repository
            .list_canonical_realms(project_id, &network_id)
            .await
            .map_err(map_store_error)
    }

    pub async fn list_canonical_realms(
        &self,
        auth: &AuthContext,
        network_id: Uuid,
    ) -> Result<Vec<o3k_store::CanonicalAddressRealmRecord>, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "ListAddressRealms",
                "address_realm",
                None,
                Some(("network", network_id)),
            )
            .await?;
        let result = self
            .list_canonical_realms_for_project(auth.effective_scope().id().as_str(), network_id)
            .await;
        let audit_result = result.as_ref().map(|_| ());
        self.audit_canonical_result(auth, namespace, action, resource_type, None, audit_result);
        result
    }

    pub async fn get_canonical_realm(
        &self,
        auth: &AuthContext,
        realm_id: Uuid,
    ) -> Result<o3k_store::CanonicalAddressRealmRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "ReadAddressRealm",
                "address_realm",
                Some(realm_id),
                None,
            )
            .await?;
        let result = self
            .inner
            .repository
            .get_canonical_realm(auth.effective_scope().id().as_str(), &realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound);
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            Some(realm_id),
            result.as_ref().map(|_| ()),
        );
        result
    }

    /// Owner-scoped lookup used by compatibility projections after the
    /// request has already been authorized at the API boundary.
    pub async fn get_canonical_realm_for_project(
        &self,
        project_id: &str,
        realm_id: Uuid,
    ) -> Result<o3k_store::CanonicalAddressRealmRecord, NetworkError> {
        self.inner
            .repository
            .get_canonical_realm(project_id, &realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)
    }

    pub async fn delete_canonical_realm_for_project(
        &self,
        project_id: &str,
        realm_id: Uuid,
    ) -> Result<(), NetworkError> {
        let progress = self
            .begin_canonical_realm_deletion_for_project(project_id, realm_id)
            .await?;
        let operation_id = match progress {
            RealmCleanupProgress::Deleting { operation_id, .. }
            | RealmCleanupProgress::AwaitingObservation { operation_id, .. }
            | RealmCleanupProgress::Removed { operation_id } => operation_id,
        };
        let bindings = self
            .inner
            .repository
            .list_canonical_realm_bindings(&realm_id)
            .await
            .map_err(map_store_error)?;
        if bindings.is_empty() {
            match self
                .observe_canonical_realm_cleanup_for_project(project_id, realm_id, Vec::new())
                .await?
            {
                RealmCleanupProgress::Removed { .. } => Ok(()),
                _ => Err(NetworkError::Conflict),
            }
        } else {
            // No provider observation is available at this service boundary;
            // retaining Deleting is the safe result. A provider/reconciler
            // must call the explicit observation method before final removal.
            let _ = operation_id;
            Err(NetworkError::Conflict)
        }
    }

    pub async fn delete_canonical_realm(
        &self,
        auth: &AuthContext,
        realm_id: Uuid,
    ) -> Result<(), NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "DeleteAddressRealm",
                "address_realm",
                Some(realm_id),
                None,
            )
            .await?;
        let result = self
            .delete_canonical_realm_for_project(auth.effective_scope().id().as_str(), realm_id)
            .await;
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            Some(realm_id),
            result.as_ref().map(|_| ()),
        );
        result
    }

    /// Durably accepts a Realm deletion before any provider mutation. The
    /// operation identity is deterministic, so retries join the same
    /// workflow even after a process restart.
    pub async fn begin_canonical_realm_deletion_for_project(
        &self,
        project_id: &str,
        realm_id: Uuid,
    ) -> Result<RealmCleanupProgress, NetworkError> {
        let _guard = self.lock.lock().await;
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        let (operation, canonical, request) = realm_delete_operation(project_id, realm_id)?;
        let accepted = self
            .inner
            .repository
            .create_or_replay_canonical_scoped_operation(&operation, &canonical, &request)
            .await
            .map_err(map_store_error)?;
        let operation_id = match accepted {
            o3k_store::IdempotencyReservation::Conflict => return Err(NetworkError::Conflict),
            o3k_store::IdempotencyReservation::Created(id)
            | o3k_store::IdempotencyReservation::ExistingEquivalent(id) => id,
        };
        if realm.state == "deleting" {
            return Ok(RealmCleanupProgress::AwaitingObservation {
                operation_id,
                generation: realm.generation,
            });
        }
        if realm.state != "active" {
            let _ = self
                .inner
                .repository
                .update_operation(
                    operation_id,
                    o3k_store::OperationState::Failed,
                    None,
                    Some("invalid_state"),
                    Some("Realm is not deletable in its current lifecycle state"),
                )
                .await;
            return Err(NetworkError::Conflict);
        }
        let deleting = match self
            .inner
            .repository
            .begin_canonical_realm_deletion(project_id, &realm_id, realm.generation)
            .await
        {
            Ok(value) => value,
            Err(error) => {
                let state = if matches!(error, o3k_store::StoreError::NetworkInUse) {
                    o3k_store::OperationState::Retryable
                } else {
                    o3k_store::OperationState::Failed
                };
                let _ = self
                    .inner
                    .repository
                    .update_operation(
                        operation_id,
                        state,
                        None,
                        Some("rejected"),
                        Some(&error.to_string()),
                    )
                    .await;
                return Err(map_store_error(error));
            }
        };
        self.inner
            .repository
            .update_resource(
                realm_id,
                i64::try_from(realm.generation).map_err(|_| NetworkError::InvalidRequest)?,
                "deleting",
                "deleting",
                i64::try_from(deleting.generation).map_err(|_| NetworkError::InvalidRequest)?,
                None,
            )
            .await
            .map_err(map_store_error)?;
        let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
            o3k_kernel::OperationState::Running,
            1,
            Some(canonical.created_at.clone()),
            None,
            None,
        )
        .map_err(map_store_error)?;
        self.inner
            .repository
            .update_canonical_operation_lifecycle(operation_id, &lifecycle)
            .await
            .map_err(map_store_error)?;
        Ok(RealmCleanupProgress::Deleting {
            operation_id,
            generation: deleting.generation,
        })
    }

    /// Applies a provider observation to a durable Realm deletion. Every
    /// current binding must be identified exactly; absent observations remove
    /// only the matching owned binding after generation validation.
    pub async fn observe_canonical_realm_cleanup_for_project(
        &self,
        project_id: &str,
        realm_id: Uuid,
        observations: Vec<RealmCleanupObservation>,
    ) -> Result<RealmCleanupProgress, NetworkError> {
        let _guard = self.lock.lock().await;
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        if realm.state != "deleting" {
            return Err(NetworkError::Conflict);
        }
        let (operation, _, _) = realm_delete_operation(project_id, realm_id)?;
        let operation_record = self
            .inner
            .repository
            .get_operation(operation.id)
            .await
            .map_err(map_store_error)?;
        let canonical = self
            .inner
            .repository
            .get_canonical_operation(operation.id)
            .await
            .map_err(map_store_error)?;
        if operation_record.resource_id != realm_id
            || canonical.owner_scope != project_id
            || canonical.resource_id.as_deref() != Some(&realm_id.to_string())
        {
            return Err(NetworkError::InvalidRequest);
        }
        let bindings = self
            .inner
            .repository
            .list_canonical_realm_bindings(&realm_id)
            .await
            .map_err(map_store_error)?;
        if observations.len() != bindings.len() {
            return Err(NetworkError::Conflict);
        }
        let same_binding =
            |left: &o3k_store::CanonicalRealmBindingRecord,
             right: &o3k_store::CanonicalRealmBindingRecord| {
                left == right && left.realm_id == realm_id
            };
        for binding in &bindings {
            let matching: Vec<_> = observations
                .iter()
                .filter(|observation| match observation {
                    RealmCleanupObservation::Absent(value)
                    | RealmCleanupObservation::Present(value) => same_binding(binding, value),
                    RealmCleanupObservation::Unknown { binding: value, .. } => {
                        same_binding(binding, value)
                    }
                })
                .collect();
            if matching.len() != 1 {
                return Err(NetworkError::Conflict);
            }
            match matching[0] {
                RealmCleanupObservation::Unknown { reason, .. } => {
                    let update = o3k_store::CanonicalOperationLifecycleUpdate::new(
                        o3k_kernel::OperationState::UnknownOutcome,
                        canonical.attempt.saturating_add(1),
                        canonical.started_at.clone(),
                        None,
                        Some(reason.clone()),
                    )
                    .map_err(map_store_error)?;
                    self.inner
                        .repository
                        .update_canonical_operation_lifecycle(operation.id, &update)
                        .await
                        .map_err(map_store_error)?;
                    return Ok(RealmCleanupProgress::AwaitingObservation {
                        operation_id: operation.id,
                        generation: realm.generation,
                    });
                }
                RealmCleanupObservation::Present(_) => {
                    let update = o3k_store::CanonicalOperationLifecycleUpdate::new(
                        o3k_kernel::OperationState::Retryable,
                        canonical.attempt.saturating_add(1),
                        canonical.started_at.clone(),
                        None,
                        Some("owned provider Realm state is still present".to_owned()),
                    )
                    .map_err(map_store_error)?;
                    self.inner
                        .repository
                        .update_canonical_operation_lifecycle(operation.id, &update)
                        .await
                        .map_err(map_store_error)?;
                    return Ok(RealmCleanupProgress::AwaitingObservation {
                        operation_id: operation.id,
                        generation: realm.generation,
                    });
                }
                RealmCleanupObservation::Absent(_) => {}
            }
        }
        for binding in bindings {
            self.inner
                .repository
                .delete_canonical_realm_binding(&binding, realm.generation)
                .await
                .map_err(map_store_error)?;
        }
        self.inner
            .repository
            .finalize_canonical_realm_deletion(project_id, &realm_id, realm.generation)
            .await
            .map_err(map_store_error)?;
        let _ = self
            .inner
            .repository
            .update_resource(
                realm_id,
                i64::try_from(realm.generation).map_err(|_| NetworkError::InvalidRequest)?,
                "deleted",
                "deleted",
                i64::try_from(realm.generation).map_err(|_| NetworkError::InvalidRequest)?,
                None,
            )
            .await;
        let update = o3k_store::CanonicalOperationLifecycleUpdate::new(
            o3k_kernel::OperationState::Succeeded,
            canonical.attempt.saturating_add(1),
            canonical.started_at.clone(),
            Some(
                canonical
                    .started_at
                    .clone()
                    .unwrap_or_else(|| canonical.created_at.clone()),
            ),
            None,
        )
        .map_err(map_store_error)?;
        self.inner
            .repository
            .update_canonical_operation_lifecycle(operation.id, &update)
            .await
            .map_err(map_store_error)?;
        Ok(RealmCleanupProgress::Removed {
            operation_id: operation.id,
        })
    }

    pub async fn create_canonical_pool_for_project(
        &self,
        project_id: &str,
        realm_id: Uuid,
        prefix: String,
        gateway: Option<Ipv4Addr>,
        first_usable: Ipv4Addr,
        last_usable: Ipv4Addr,
    ) -> Result<o3k_store::CanonicalAddressPoolRecord, NetworkError> {
        let pool_prefix_net = Ipv4Net::parse(&prefix)?;
        let pool_prefix = pool_prefix_net.canonical();
        if first_usable > last_usable
            || !pool_prefix_net.contains(first_usable)
            || !pool_prefix_net.contains(last_usable)
        {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock.lock().await;
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        if realm.state != "active" {
            return Err(NetworkError::Conflict);
        }
        let realm_prefix = Ipv4Net::parse(&realm.prefix)?;
        if !realm_prefix.contains(pool_prefix_net.network)
            || pool_prefix_net.prefix < realm_prefix.prefix
        {
            return Err(NetworkError::InvalidRequest);
        }
        if gateway.is_some_and(|value| !pool_prefix_net.contains(value)) {
            return Err(NetworkError::InvalidRequest);
        }
        let pool = o3k_store::CanonicalAddressPoolRecord {
            id: Uuid::now_v7(),
            realm_id,
            project_id: project_id.to_owned(),
            prefix: pool_prefix,
            gateway,
            first_usable,
            last_usable,
            generation: 1,
            state: "active".to_owned(),
        };
        self.inner
            .repository
            .insert_canonical_pool(&pool)
            .await
            .map_err(map_store_error)?;
        Ok(pool)
    }

    pub async fn create_canonical_pool(
        &self,
        auth: &AuthContext,
        realm_id: Uuid,
        prefix: String,
        gateway: Option<Ipv4Addr>,
        first_usable: Ipv4Addr,
        last_usable: Ipv4Addr,
    ) -> Result<o3k_store::CanonicalAddressPoolRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "CreateAddressPool",
                "address_pool",
                None,
                Some(("address_realm", realm_id)),
            )
            .await?;
        let result = self
            .create_canonical_pool_for_project(
                auth.effective_scope().id().as_str(),
                realm_id,
                prefix,
                gateway,
                first_usable,
                last_usable,
            )
            .await;
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            None,
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn list_canonical_pools(
        &self,
        auth: &AuthContext,
        realm_id: Uuid,
    ) -> Result<Vec<o3k_store::CanonicalAddressPoolRecord>, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "ListAddressPools",
                "address_pool",
                None,
                Some(("address_realm", realm_id)),
            )
            .await?;
        let result = self
            .inner
            .repository
            .list_canonical_pools(auth.effective_scope().id().as_str(), &realm_id)
            .await
            .map_err(map_store_error);
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            None,
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn delete_canonical_pool(
        &self,
        auth: &AuthContext,
        pool_id: Uuid,
    ) -> Result<(), NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "DeleteAddressPool",
                "address_pool",
                Some(pool_id),
                None,
            )
            .await?;
        let result = self
            .inner
            .repository
            .delete_canonical_pool(auth.effective_scope().id().as_str(), &pool_id)
            .await
            .map_err(map_store_error);
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            Some(pool_id),
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn create_canonical_endpoint_for_project(
        &self,
        project_id: &str,
        realm_id: Uuid,
        fixed_ip: Ipv4Addr,
        mac: String,
    ) -> Result<o3k_store::CanonicalEndpointRecord, NetworkError> {
        if mac.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock.lock().await;
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        if !Ipv4Net::parse(&realm.prefix)?.contains(fixed_ip) || realm.state != "active" {
            return Err(NetworkError::InvalidRequest);
        }
        let endpoint = o3k_store::CanonicalEndpointRecord {
            id: Uuid::now_v7(),
            realm_id,
            project_id: project_id.to_owned(),
            fixed_ip,
            mac,
            generation: 1,
            state: "active".to_owned(),
        };
        self.inner
            .repository
            .insert_canonical_endpoint(&endpoint)
            .await
            .map_err(map_store_error)?;
        Ok(endpoint)
    }

    pub async fn create_canonical_endpoint(
        &self,
        auth: &AuthContext,
        realm_id: Uuid,
        fixed_ip: Ipv4Addr,
        mac: String,
    ) -> Result<o3k_store::CanonicalEndpointRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "CreateEndpoint",
                "endpoint",
                None,
                Some(("address_realm", realm_id)),
            )
            .await?;
        let result = self
            .create_canonical_endpoint_for_project(
                auth.effective_scope().id().as_str(),
                realm_id,
                fixed_ip,
                mac,
            )
            .await;
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            None,
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn list_canonical_endpoints(
        &self,
        auth: &AuthContext,
        realm_id: Uuid,
    ) -> Result<Vec<o3k_store::CanonicalEndpointRecord>, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "ListEndpoints",
                "endpoint",
                None,
                Some(("address_realm", realm_id)),
            )
            .await?;
        let result = self
            .inner
            .repository
            .list_canonical_endpoints(auth.effective_scope().id().as_str(), &realm_id)
            .await
            .map_err(map_store_error);
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            None,
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn get_canonical_endpoint(
        &self,
        auth: &AuthContext,
        endpoint_id: Uuid,
    ) -> Result<o3k_store::CanonicalEndpointRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(auth, "ReadEndpoint", "endpoint", Some(endpoint_id), None)
            .await?;
        let result = self
            .inner
            .repository
            .get_canonical_endpoint(auth.effective_scope().id().as_str(), &endpoint_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound);
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            Some(endpoint_id),
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn delete_canonical_endpoint(
        &self,
        auth: &AuthContext,
        endpoint_id: Uuid,
    ) -> Result<(), NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(auth, "DeleteEndpoint", "endpoint", Some(endpoint_id), None)
            .await?;
        let result = self
            .inner
            .repository
            .delete_canonical_endpoint(auth.effective_scope().id().as_str(), &endpoint_id)
            .await
            .map_err(map_store_error);
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            Some(endpoint_id),
            result.as_ref().map(|_| ()),
        );
        result
    }

    /// Reconstructs canonical execution inputs from durable rows. Empty child
    /// collections are valid and never become NotFound or placeholder realms.
    pub async fn reconstruct_canonical_network(
        &self,
        project_id: &str,
        network_id: Uuid,
    ) -> Result<CanonicalNetworkSnapshot, NetworkError> {
        let network = self
            .get_canonical_network_for_project(project_id, network_id)
            .await?;
        let realms = self
            .inner
            .repository
            .list_canonical_realms(project_id, &network_id)
            .await
            .map_err(map_store_error)?;
        let mut pools = BTreeMap::new();
        let mut endpoints = BTreeMap::new();
        for realm in &realms {
            if realm.network_id != network.id || realm.project_id != network.project_id {
                return Err(NetworkError::InvalidRequest);
            }
            pools.insert(
                realm.id,
                self.inner
                    .repository
                    .list_canonical_pools(project_id, &realm.id)
                    .await
                    .map_err(map_store_error)?,
            );
            endpoints.insert(
                realm.id,
                self.inner
                    .repository
                    .list_canonical_endpoints(project_id, &realm.id)
                    .await
                    .map_err(map_store_error)?,
            );
        }
        let realm_ids: BTreeSet<Uuid> = realms.iter().map(|realm| realm.id).collect();
        let mut l3_gateways = Vec::new();
        for gateway in self
            .inner
            .repository
            .list_canonical_l3_gateways(project_id)
            .await
            .map_err(map_store_error)?
        {
            let attachments = self
                .inner
                .repository
                .list_canonical_l3_gateway_attachments(project_id, &gateway.id)
                .await
                .map_err(map_store_error)?;
            let relevant: Vec<_> = attachments
                .into_iter()
                .filter(|attachment| realm_ids.contains(&attachment.realm_id))
                .collect();
            if !relevant.is_empty() {
                l3_gateways.push((gateway, relevant));
            }
        }
        Ok(CanonicalNetworkSnapshot {
            network,
            realms,
            pools,
            endpoints,
            l3_gateways,
        })
    }

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
    ) -> Result<(), NetworkError> {
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
        for group_id in &group_ids {
            if self
                .inner
                .repository
                .get_reusable_policy(project_id, group_id)
                .await
                .map_err(map_store_error)?
                .is_none()
            {
                return Err(NetworkError::NotFound);
            }
        }
        let existing = self
            .inner
            .repository
            .list_endpoint_policy_attachments(project_id, &endpoint_id)
            .await
            .map_err(map_store_error)?;
        for attachment in existing.into_iter().filter(|a| a.state == "active") {
            self.inner
                .repository
                .delete_policy_attachment(project_id, &attachment.id)
                .await
                .map_err(map_store_error)?;
        }
        for group_id in group_ids {
            self.inner
                .repository
                .insert_policy_attachment(&o3k_store::CanonicalPolicyAttachmentRecord {
                    id: Uuid::now_v7(),
                    policy_id: group_id,
                    endpoint_id,
                    project_id: project_id.to_owned(),
                    state: "active".to_owned(),
                    generation: 1,
                })
                .await
                .map_err(map_store_error)?;
        }
        Ok(())
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

    #[allow(clippy::too_many_arguments)]
    pub async fn create_subnet(
        &self,
        auth: &AuthContext,
        network_id: Uuid,
        name: String,
        cidr: String,
        gateway_ip: Option<Ipv4Addr>,
        allocation_start: Option<Ipv4Addr>,
        allocation_end: Option<Ipv4Addr>,
    ) -> Result<SubnetRecord, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "CreateSubnet").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "CreateSubnet".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("network", "subnet").map_err(|_| NetworkError::InvalidRequest)?,
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
            .create_subnet_for_project(
                auth.effective_scope().id().as_str(),
                network_id,
                name,
                cidr,
                gateway_ip,
                allocation_start,
                allocation_end,
            )
            .await
        {
            Ok(record) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("network", "subnet").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("network".to_owned(), "subnet".to_owned())
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

    #[allow(clippy::too_many_arguments)]
    pub async fn create_subnet_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
        name: String,
        cidr: String,
        gateway_ip: Option<Ipv4Addr>,
        allocation_start: Option<Ipv4Addr>,
        allocation_end: Option<Ipv4Addr>,
    ) -> Result<SubnetRecord, NetworkError> {
        let net = Ipv4Net::parse(&cidr)?;
        let cidr = net.canonical();
        let gateway = gateway_ip.unwrap_or(net.first_host());
        if !net.contains(gateway) || gateway == net.network || gateway == net.broadcast {
            return Err(NetworkError::InvalidRequest);
        }
        let start = allocation_start.unwrap_or(Ipv4Addr::from(u32::from(net.first_host()) + 1));
        let end = allocation_end.unwrap_or(net.last_host());
        if !net.contains(start)
            || !net.contains(end)
            || start > end
            || (u32::from(start)..=u32::from(end)).contains(&u32::from(gateway))
        {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        self.get_canonical_network_for_project(project_id, network_id)
            .await?;
        let subnet = SubnetRecord {
            id: Uuid::now_v7(),
            network_id,
            name,
            project_id: project_id.to_owned(),
            cidr,
            gateway_ip: gateway,
            allocation_start: start,
            allocation_end: end,
            ip_version: 4,
            enable_dhcp: true,
        };
        let scope =
            OwnershipScope::project(ScopeId::new_unchecked(project_id.to_owned()), None, None);
        let amounts = vec![ResourceAmount::new(LimitKey::network_subnets(), 1)];
        let op_id = format!("o3k:subnet:create:{}:{}", project_id, subnet.id);
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

        if self
            .inner
            .repository
            .list_canonical_realms(project_id, &network_id)
            .await
            .map_err(map_store_error)?
            .iter()
            .any(|realm| realm.state == "active")
        {
            let _ = self
                .inner
                .repository
                .release_reservation(&quota_res.id)
                .await;
            return Err(NetworkError::Conflict);
        }

        let realm = o3k_store::CanonicalAddressRealmRecord {
            id: subnet.id,
            network_id: subnet.network_id,
            project_id: subnet.project_id.clone(),
            prefix: subnet.cidr.clone(),
            overlapping_prefixes: false,
            generation: 1,
            state: "active".to_owned(),
        };
        let pool = o3k_store::CanonicalAddressPoolRecord {
            id: Uuid::now_v7(),
            realm_id: realm.id,
            project_id: realm.project_id.clone(),
            prefix: realm.prefix.clone(),
            gateway: Some(subnet.gateway_ip),
            first_usable: subnet.allocation_start,
            last_usable: subnet.allocation_end,
            generation: 1,
            state: "active".to_owned(),
        };
        match self
            .inner
            .repository
            .insert_subnet_bundle(&realm, &pool, &subnet)
            .await
        {
            Ok(()) => {
                let _ = self
                    .inner
                    .repository
                    .commit_reservation(&quota_res.id)
                    .await;
                Ok(subnet)
            }
            Err(o3k_store::StoreError::NetworkInUse)
            | Err(o3k_store::StoreError::ResourceAlreadyExists) => {
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

    pub async fn list_subnets(
        &self,
        auth: &AuthContext,
    ) -> Result<Vec<SubnetRecord>, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "ListSubnets").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "ListSubnets".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("network", "subnet").map_err(|_| NetworkError::InvalidRequest)?,
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
        self.list_subnets_for_project(auth.effective_scope().id().as_str())
            .await
    }

    pub async fn list_subnets_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<SubnetRecord>, NetworkError> {
        let networks = self
            .inner
            .repository
            .list_canonical_networks(project_id)
            .await
            .map_err(map_store_error)?;
        let mut result = Vec::new();
        for network in networks {
            for realm in self
                .inner
                .repository
                .list_canonical_realms(project_id, &network.id)
                .await
                .map_err(map_store_error)?
            {
                result.push(self.project_canonical_subnet(project_id, &realm).await?);
            }
        }
        Ok(result)
    }

    pub async fn get_subnet(
        &self,
        auth: &AuthContext,
        id: Uuid,
    ) -> Result<SubnetRecord, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "ReadSubnet").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "ReadSubnet".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("network", "subnet").map_err(|_| NetworkError::InvalidRequest)?,
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
        self.get_subnet_for_project(auth.effective_scope().id().as_str(), id)
            .await
    }

    pub async fn get_subnet_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<SubnetRecord, NetworkError> {
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        self.project_canonical_subnet(project_id, &realm).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_subnet(
        &self,
        auth: &AuthContext,
        id: Uuid,
        name: Option<String>,
        gateway_ip: Option<Ipv4Addr>,
        enable_dhcp: Option<bool>,
        network_id: Option<Uuid>,
        cidr: Option<String>,
        ip_version: Option<u8>,
    ) -> Result<SubnetRecord, NetworkError> {
        let action = ActionId::new("network", "UpdateSubnet").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "UpdateSubnet".to_owned())
        });
        let request = AuthorizationRequest {
            auth_context: auth,
            action,
            resource_target: ResourceTarget::instance(
                ResourceType::new("network", "subnet").map_err(|_| NetworkError::InvalidRequest)?,
                ResourceId::new(id.to_string()).map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        if !self.authorizer.authorize(&request).is_allowed() {
            return Err(NetworkError::NotFound);
        }
        if network_id.is_some() || cidr.is_some() || ip_version.is_some_and(|v| v != 4) {
            return Err(NetworkError::InvalidRequest);
        }
        self.update_subnet_for_project(
            auth.effective_scope().id().as_str(),
            id,
            name,
            gateway_ip,
            enable_dhcp,
        )
        .await
    }

    async fn update_subnet_for_project(
        &self,
        project_id: &str,
        id: Uuid,
        name: Option<String>,
        gateway_ip: Option<Ipv4Addr>,
        enable_dhcp: Option<bool>,
    ) -> Result<SubnetRecord, NetworkError> {
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        let pool = self
            .inner
            .repository
            .list_canonical_pools(project_id, &id)
            .await
            .map_err(map_store_error)?
            .into_iter()
            .next()
            .ok_or(NetworkError::InvalidRequest)?;
        let current = self.project_canonical_subnet(project_id, &realm).await?;
        let gateway = gateway_ip.unwrap_or(current.gateway_ip);
        let net = Ipv4Net::parse(&realm.prefix)?;
        if !net.contains(gateway) || gateway == net.network || gateway == net.broadcast {
            return Err(NetworkError::InvalidRequest);
        }
        if (u32::from(pool.first_usable)..=u32::from(pool.last_usable))
            .contains(&u32::from(gateway))
        {
            return Err(NetworkError::Conflict);
        }
        let updated = SubnetRecord {
            name: name.unwrap_or(current.name),
            gateway_ip: gateway,
            enable_dhcp: enable_dhcp.unwrap_or(current.enable_dhcp),
            ..current
        };
        if gateway != pool.gateway.unwrap_or(gateway) {
            self.inner
                .repository
                .update_subnet_bundle(&updated, &pool.id, pool.generation)
                .await
                .map_err(map_store_error)?;
        } else {
            self.inner
                .repository
                .update_subnet(&updated)
                .await
                .map_err(map_store_error)?;
        }
        self.get_subnet_for_project(project_id, id).await
    }

    async fn project_canonical_subnet(
        &self,
        project_id: &str,
        realm: &o3k_store::CanonicalAddressRealmRecord,
    ) -> Result<SubnetRecord, NetworkError> {
        let pool = self
            .inner
            .repository
            .list_canonical_pools(project_id, &realm.id)
            .await
            .map_err(map_store_error)?
            .into_iter()
            .next()
            .ok_or(NetworkError::InvalidRequest)?;
        let metadata = self
            .inner
            .repository
            .get_subnet(project_id, &realm.id)
            .await
            .map_err(map_store_error)?;
        Ok(SubnetRecord {
            id: realm.id,
            network_id: realm.network_id,
            name: metadata
                .as_ref()
                .map(|value| value.name.clone())
                .unwrap_or_default(),
            project_id: realm.project_id.clone(),
            cidr: realm.prefix.clone(),
            gateway_ip: pool.gateway.ok_or(NetworkError::InvalidRequest)?,
            allocation_start: pool.first_usable,
            allocation_end: pool.last_usable,
            ip_version: 4,
            enable_dhcp: metadata
                .as_ref()
                .map(|value| value.enable_dhcp)
                .unwrap_or(true),
        })
    }

    pub async fn delete_subnet(&self, auth: &AuthContext, id: Uuid) -> Result<(), NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "DeleteSubnet").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "DeleteSubnet".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("network", "subnet").map_err(|_| NetworkError::InvalidRequest)?,
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
            .delete_subnet_for_project(auth.effective_scope().id().as_str(), id)
            .await
        {
            Ok(()) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("network", "subnet").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("network".to_owned(), "subnet".to_owned())
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

    pub async fn delete_subnet_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        let metadata_exists = self
            .inner
            .repository
            .get_subnet(project_id, &id)
            .await
            .map_err(map_store_error)?
            .is_some();
        let realm_exists = self
            .inner
            .repository
            .get_canonical_realm(project_id, &id)
            .await
            .map_err(map_store_error)?
            .is_some();
        if !metadata_exists && !realm_exists {
            return Err(NetworkError::NotFound);
        }
        self.inner
            .repository
            .delete_subnet_bundle(project_id, &id)
            .await
            .map_err(map_store_error)?;
        let _ = self
            .inner
            .repository
            .release_reservation_for_operation(&format!("o3k:subnet:create:{}:{}", project_id, id))
            .await;
        Ok(())
    }

    pub async fn create_port(
        &self,
        auth: &AuthContext,
        network_id: Uuid,
        name: String,
    ) -> Result<PortRecord, NetworkError> {
        self.create_port_with_fixed_ip(auth, network_id, name, None)
            .await
    }

    pub async fn create_port_with_fixed_ip(
        &self,
        auth: &AuthContext,
        network_id: Uuid,
        name: String,
        requested_fixed_ip: Option<(Uuid, Option<Ipv4Addr>)>,
    ) -> Result<PortRecord, NetworkError> {
        if name.starts_with("o3k-server:") {
            return Err(NetworkError::InvalidRequest);
        }
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "CreatePort").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "CreatePort".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("network", "port").map_err(|_| NetworkError::InvalidRequest)?,
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
            .create_port_for_project_with_fixed_ip(
                auth.effective_scope().id().as_str(),
                network_id,
                name,
                requested_fixed_ip,
            )
            .await
        {
            Ok(record) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("network", "port").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("network".to_owned(), "port".to_owned())
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

    pub async fn create_port_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
        name: String,
    ) -> Result<PortRecord, NetworkError> {
        self.create_port_for_project_with_fixed_ip(project_id, network_id, name, None)
            .await
    }

    pub async fn create_port_for_project_with_fixed_ip(
        &self,
        project_id: &str,
        network_id: Uuid,
        name: String,
        requested_fixed_ip: Option<(Uuid, Option<Ipv4Addr>)>,
    ) -> Result<PortRecord, NetworkError> {
        self.get_canonical_network_for_project(project_id, network_id)
            .await?;
        let realms = self
            .inner
            .repository
            .list_canonical_realms(project_id, &network_id)
            .await
            .map_err(map_store_error)?;
        let realm = if let Some((subnet_id, _)) = requested_fixed_ip {
            realms
                .into_iter()
                .find(|realm| realm.id == subnet_id && realm.state == "active")
                .ok_or(NetworkError::NotFound)?
        } else {
            match realms.as_slice() {
                [] => return Err(NetworkError::NotFound),
                [realm] if realm.state == "active" => realm.clone(),
                [_] => return Err(NetworkError::Conflict),
                _ => return Err(NetworkError::InvalidRequest),
            }
        };
        let pool = self
            .inner
            .repository
            .list_canonical_pools(project_id, &realm.id)
            .await
            .map_err(map_store_error)?
            .into_iter()
            .next()
            .ok_or(NetworkError::NotFound)?;
        let explicit_ip = requested_fixed_ip.and_then(|(_, ip)| ip);
        let mut candidate = explicit_ip
            .map(u32::from)
            .unwrap_or_else(|| u32::from(pool.first_usable));
        let end = explicit_ip
            .map(u32::from)
            .unwrap_or_else(|| u32::from(pool.last_usable));
        let gateway = pool.gateway.ok_or(NetworkError::InvalidRequest)?;
        while candidate <= end {
            let address = Ipv4Addr::from(candidate);
            if address != gateway
                && candidate >= u32::from(pool.first_usable)
                && candidate <= u32::from(pool.last_usable)
            {
                let id = Uuid::now_v7();
                let port = PortRecord {
                    id,
                    network_id,
                    subnet_id: Some(realm.id),
                    project_id: project_id.to_owned(),
                    name: name.clone(),
                    mac_address: deterministic_port_mac(id),
                    fixed_ip: address,
                    status: "ACTIVE".to_owned(),
                    binding_host: None,
                    binding_state: None,
                };
                let scope = OwnershipScope::project(
                    ScopeId::new_unchecked(project_id.to_owned()),
                    None,
                    None,
                );
                let amounts = vec![ResourceAmount::new(LimitKey::network_ports(), 1)];
                let op_id = format!("o3k:port:create:{}:{}", project_id, port.id);
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

                let endpoint = o3k_store::CanonicalEndpointRecord {
                    id: port.id,
                    realm_id: realm.id,
                    project_id: project_id.to_owned(),
                    fixed_ip: port.fixed_ip,
                    mac: port.mac_address.clone(),
                    generation: 1,
                    state: "active".to_owned(),
                };
                let mut insert_result = Err(o3k_store::StoreError::ResourceNotFound);
                for _ in 0..8 {
                    insert_result = self
                        .inner
                        .repository
                        .insert_canonical_endpoint_and_port(&endpoint, &port)
                        .await;
                    if !insert_result
                        .as_ref()
                        .is_err_and(|error| error.to_string().contains("database is locked"))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                match insert_result {
                    Ok(()) => {
                        let _ = self
                            .inner
                            .repository
                            .commit_reservation(&quota_res.id)
                            .await;
                        return Ok(port);
                    }
                    Err(o3k_store::StoreError::ResourceAlreadyExists) => {
                        let _ = self
                            .inner
                            .repository
                            .release_reservation(&quota_res.id)
                            .await;
                        if explicit_ip.is_some() {
                            return Err(NetworkError::Conflict);
                        }
                    }
                    Err(error) => {
                        let _ = self
                            .inner
                            .repository
                            .release_reservation(&quota_res.id)
                            .await;
                        return Err(map_store_error(error));
                    }
                }
            }
            if explicit_ip.is_some() {
                break;
            }
            candidate = candidate.saturating_add(1);
        }
        if explicit_ip.is_some() {
            Err(NetworkError::InvalidRequest)
        } else {
            Err(NetworkError::PoolExhausted)
        }
    }

    pub async fn list_ports(&self, auth: &AuthContext) -> Result<Vec<PortRecord>, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "ListPorts").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "ListPorts".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("network", "port").map_err(|_| NetworkError::InvalidRequest)?,
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
        self.list_ports_for_project(auth.effective_scope().id().as_str())
            .await
    }

    pub async fn list_ports_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<PortRecord>, NetworkError> {
        let networks = self
            .inner
            .repository
            .list_canonical_networks(project_id)
            .await
            .map_err(map_store_error)?;
        let mut result = Vec::new();
        for network in networks {
            for realm in self
                .inner
                .repository
                .list_canonical_realms(project_id, &network.id)
                .await
                .map_err(map_store_error)?
            {
                for endpoint in self
                    .inner
                    .repository
                    .list_canonical_endpoints(project_id, &realm.id)
                    .await
                    .map_err(map_store_error)?
                {
                    result.push(
                        self.project_canonical_port(project_id, &realm, &endpoint)
                            .await?,
                    );
                }
            }
        }
        Ok(result)
    }

    pub async fn get_port(&self, auth: &AuthContext, id: Uuid) -> Result<PortRecord, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "ReadPort").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "ReadPort".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("network", "port").map_err(|_| NetworkError::InvalidRequest)?,
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
        self.get_port_for_project(auth.effective_scope().id().as_str(), id)
            .await
    }

    pub async fn get_port_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<PortRecord, NetworkError> {
        let endpoint = self
            .inner
            .repository
            .get_canonical_endpoint(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &endpoint.realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        self.project_canonical_port(project_id, &realm, &endpoint)
            .await
    }

    pub async fn update_port_name_for_project(
        &self,
        project_id: &str,
        id: Uuid,
        name: String,
    ) -> Result<PortRecord, NetworkError> {
        let current = self.get_port_for_project(project_id, id).await?;
        if current.name.starts_with("o3k-server:") {
            return Err(NetworkError::Conflict);
        }
        self.inner
            .repository
            .update_port_name(project_id, &id, &name)
            .await
            .map_err(map_store_error)?;
        self.get_port_for_project(project_id, id).await
    }

    async fn project_canonical_port(
        &self,
        project_id: &str,
        realm: &o3k_store::CanonicalAddressRealmRecord,
        endpoint: &o3k_store::CanonicalEndpointRecord,
    ) -> Result<PortRecord, NetworkError> {
        let metadata = self
            .inner
            .repository
            .get_port(project_id, &endpoint.id)
            .await
            .map_err(map_store_error)?;
        Ok(PortRecord {
            id: endpoint.id,
            network_id: realm.network_id,
            subnet_id: Some(realm.id),
            project_id: endpoint.project_id.clone(),
            name: metadata
                .as_ref()
                .map(|value| value.name.clone())
                .unwrap_or_default(),
            mac_address: endpoint.mac.clone(),
            fixed_ip: endpoint.fixed_ip,
            status: endpoint.state.to_ascii_uppercase(),
            binding_host: metadata
                .as_ref()
                .and_then(|value| value.binding_host.clone()),
            binding_state: metadata.and_then(|value| value.binding_state),
        })
    }

    /// Internal owner lookup used by canonical dependency authorization. It
    /// is not exposed as a tenant-facing read path and carries no metadata to
    /// the caller beyond the durable owner record.
    pub async fn find_port_by_id(&self, id: Uuid) -> Result<Option<PortRecord>, NetworkError> {
        self.inner
            .repository
            .get_port_by_id(&id)
            .await
            .map_err(map_store_error)
    }

    pub async fn delete_port(&self, auth: &AuthContext, id: Uuid) -> Result<(), NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "DeletePort").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "DeletePort".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("network", "port").map_err(|_| NetworkError::InvalidRequest)?,
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
            .delete_port_for_project(auth.effective_scope().id().as_str(), id)
            .await
        {
            Ok(()) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("network", "port").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("network".to_owned(), "port".to_owned())
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

    pub async fn delete_port_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), NetworkError> {
        // Endpoint deletion owns only the endpoint and its canonical
        // attachment relations.  Remove those relations explicitly before
        // the endpoint row so the reusable policy and its rules remain
        // independent and the endpoint delete cannot leave dangling
        // attachments.
        let attachments = self
            .inner
            .repository
            .list_endpoint_policy_attachments(project_id, &id)
            .await
            .map_err(map_store_error)?;
        for attachment in attachments {
            self.inner
                .repository
                .delete_policy_attachment(project_id, &attachment.id)
                .await
                .map_err(map_store_error)?;
        }
        self.inner
            .repository
            .delete_canonical_endpoint_and_port(project_id, &id)
            .await
            .map_err(map_store_error)?;
        let _ = self
            .inner
            .repository
            .release_reservation_for_operation(&format!("o3k:port:create:{}:{}", project_id, id))
            .await;
        Ok(())
    }

    pub async fn record_binding_intent(
        &self,
        project_id: &str,
        port_id: Uuid,
        host: &str,
    ) -> Result<PortRecord, NetworkError> {
        if host.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        let port = self
            .inner
            .repository
            .get_port(project_id, &port_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        if port
            .binding_host
            .as_deref()
            .is_some_and(|current| current != host)
        {
            return Err(NetworkError::Conflict);
        }
        // A create dispatch is underway: transitions from unbound, binding,
        // down, and error to binding. A completed `bound` observation is kept:
        // idempotent dispatch replays of an already-succeeded create must not
        // downgrade durable observed state.
        let next = match port
            .binding_state
            .as_deref()
            .and_then(PortBindingState::parse)
        {
            Some(PortBindingState::Bound) => PortBindingState::Bound,
            _ => PortBindingState::Binding,
        };
        self.inner
            .repository
            .update_port_binding(project_id, &port_id, Some(host), Some(next.as_str()))
            .await
            .map_err(map_store_error)
    }

    pub async fn project_binding_observation(
        &self,
        project_id: &str,
        port_id: Uuid,
        host: &str,
        state: &str,
    ) -> Result<PortRecord, NetworkError> {
        let state = PortBindingState::parse(state).ok_or(NetworkError::InvalidRequest)?;
        let _guard = self.lock().await;
        let port = self
            .inner
            .repository
            .get_port(project_id, &port_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        if port.binding_host.as_deref() != Some(host) {
            return Err(NetworkError::Conflict);
        }
        self.inner
            .repository
            .update_port_binding(project_id, &port_id, Some(host), Some(state.as_str()))
            .await
            .map_err(map_store_error)
    }

    /// Projects a terminal create outcome onto the port's binding using the
    /// host recorded by the dispatch intent. The durable intent is
    /// authoritative: the control plane selects the host, so a stale or
    /// mismatched caller identity cannot override it. A port without a
    /// recorded intent (never dispatched) rejects the projection.
    pub async fn project_create_outcome(
        &self,
        project_id: &str,
        port_id: Uuid,
        state: PortBindingState,
    ) -> Result<PortRecord, NetworkError> {
        if !matches!(state, PortBindingState::Bound | PortBindingState::Error) {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        let port = self
            .inner
            .repository
            .get_port(project_id, &port_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        let host = port.binding_host.as_deref().ok_or(NetworkError::Conflict)?;
        self.inner
            .repository
            .update_port_binding(project_id, &port_id, Some(host), Some(state.as_str()))
            .await
            .map_err(map_store_error)
    }

    /// Clears the binding of a port whose server reached terminal deletion.
    /// Idempotent: unbinding a port with no intent is a successful no-op.
    pub async fn unbind_port(
        &self,
        project_id: &str,
        port_id: Uuid,
    ) -> Result<PortRecord, NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .get_port(project_id, &port_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        self.inner
            .repository
            .update_port_binding(project_id, &port_id, None, None)
            .await
            .map_err(map_store_error)
    }

    async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lock.lock().await
    }
}

#[derive(Clone, Copy)]
struct Ipv4Net {
    network: Ipv4Addr,
    broadcast: Ipv4Addr,
    prefix: u8,
}

impl Ipv4Net {
    fn parse(value: &str) -> Result<Self, NetworkError> {
        let (address, prefix) = value.split_once('/').ok_or(NetworkError::InvalidRequest)?;
        let address: Ipv4Addr = address.parse().map_err(|_| NetworkError::InvalidRequest)?;
        let prefix: u8 = prefix.parse().map_err(|_| NetworkError::InvalidRequest)?;
        if prefix > 30 {
            return Err(NetworkError::InvalidRequest);
        }
        let raw = u32::from(address);
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        };
        let network = Ipv4Addr::from(raw & mask);
        let broadcast = Ipv4Addr::from((raw & mask) | !mask);
        Ok(Self {
            network,
            broadcast,
            prefix,
        })
    }

    fn canonical(self) -> String {
        format!("{}/{}", self.network, self.prefix)
    }

    fn contains(self, address: Ipv4Addr) -> bool {
        let raw = u32::from(address);
        raw >= u32::from(self.network) && raw <= u32::from(self.broadcast)
    }
    fn first_host(self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.network) + 1)
    }
    fn last_host(self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.broadcast) - 1)
    }
}

/// The legacy `metadata.json` shape written by previous versions. It is
/// parsed once, imported into the durable store, and the file is renamed so
/// it is never read again.
#[derive(serde::Deserialize)]
struct LegacyFile {
    networks: Vec<LegacyNetwork>,
    subnets: Vec<LegacySubnet>,
    ports: Vec<LegacyPort>,
}

#[derive(serde::Deserialize)]
struct LegacyNetwork {
    id: Uuid,
    name: String,
    project_id: String,
    status: String,
}

#[derive(serde::Deserialize)]
struct LegacySubnet {
    id: Uuid,
    network_id: Uuid,
    name: String,
    project_id: String,
    cidr: String,
    gateway_ip: Ipv4Addr,
    allocation_start: Ipv4Addr,
    allocation_end: Ipv4Addr,
}

#[derive(serde::Deserialize)]
struct LegacyPort {
    id: Uuid,
    network_id: Uuid,
    #[serde(default)]
    subnet_id: Uuid,
    project_id: String,
    name: String,
    #[serde(default)]
    mac_address: String,
    fixed_ip: Ipv4Addr,
    status: String,
}

/// Imports the legacy `metadata.json` file exactly once, in dependency order
/// (networks, then subnets, then ports), and renames it so `open` never reads
/// it again. The rename is best-effort: when it fails, the next `open`
/// re-reads the file, but the import is idempotent (records already present
/// are skipped), so the file can never double-import. Inserts skip records
/// that are already present, which makes a partially completed previous
/// import crash-resume safe. A corrupt file, duplicate MACs, or any
/// non-already-exists insert error fails the import closed and leaves the
/// file in place.
async fn import_legacy_metadata(
    root: &Path,
    repository: &dyn o3k_store::NetworkRepository,
) -> Result<(), NetworkError> {
    let path = root.join("metadata.json");
    let file = fs::File::open(&path)
        .map_err(|error| NetworkError::CorruptMetadata(serde_json::Error::io(error)))?;
    let mut legacy: LegacyFile =
        serde_json::from_reader(file).map_err(NetworkError::CorruptMetadata)?;
    let mut macs = HashSet::new();
    for port in &mut legacy.ports {
        if port.mac_address.is_empty() {
            port.mac_address = deterministic_port_mac(port.id);
        }
        if port.subnet_id.is_nil()
            && let Some(subnet) = legacy.subnets.iter().find(|subnet| {
                subnet.network_id == port.network_id && subnet.project_id == port.project_id
            })
        {
            port.subnet_id = subnet.id;
        }
        if !macs.insert(port.mac_address.to_ascii_lowercase()) {
            return Err(NetworkError::Conflict);
        }
    }
    for network in &legacy.networks {
        let record = NetworkRecord {
            id: network.id,
            name: network.name.clone(),
            project_id: network.project_id.clone(),
            status: network.status.clone(),
        };
        match repository.insert_network(&record).await {
            Ok(()) | Err(o3k_store::StoreError::ResourceAlreadyExists) => {}
            Err(error) => return Err(map_store_error(error)),
        }
    }
    for subnet in &legacy.subnets {
        let record = SubnetRecord {
            id: subnet.id,
            network_id: subnet.network_id,
            name: subnet.name.clone(),
            project_id: subnet.project_id.clone(),
            cidr: subnet.cidr.clone(),
            gateway_ip: subnet.gateway_ip,
            allocation_start: subnet.allocation_start,
            allocation_end: subnet.allocation_end,
            ip_version: 4,
            enable_dhcp: true,
        };
        match repository.insert_subnet(&record).await {
            Ok(()) | Err(o3k_store::StoreError::ResourceAlreadyExists) => {}
            Err(error) => return Err(map_store_error(error)),
        }
    }
    for port in &legacy.ports {
        let record = PortRecord {
            id: port.id,
            network_id: port.network_id,
            subnet_id: (!port.subnet_id.is_nil()).then_some(port.subnet_id),
            project_id: port.project_id.clone(),
            name: port.name.clone(),
            mac_address: port.mac_address.clone(),
            fixed_ip: port.fixed_ip,
            status: port.status.clone(),
            binding_host: None,
            binding_state: None,
        };
        match repository.insert_port(&record).await {
            Ok(()) | Err(o3k_store::StoreError::ResourceAlreadyExists) => {}
            Err(error) => return Err(map_store_error(error)),
        }
    }
    let _ = fs::rename(&path, root.join("metadata.json.imported"));
    Ok(())
}

fn parse_security_group_prefix(value: &str) -> Result<Ipv4Prefix, NetworkError> {
    let (address, length) = value.split_once('/').ok_or(NetworkError::InvalidRequest)?;
    let address = address.parse().map_err(|_| NetworkError::InvalidRequest)?;
    let length = length.parse().map_err(|_| NetworkError::InvalidRequest)?;
    Ipv4Prefix::new(address, length).ok_or(NetworkError::InvalidRequest)
}

fn parse_security_group_direction(value: &str) -> Result<PolicyDirection, NetworkError> {
    match value.to_ascii_lowercase().as_str() {
        "ingress" => Ok(PolicyDirection::Ingress),
        "egress" => Ok(PolicyDirection::Egress),
        _ => Err(NetworkError::InvalidRequest),
    }
}

fn parse_security_group_protocol(value: &str) -> Result<NetworkProtocol, NetworkError> {
    match value.to_ascii_lowercase().as_str() {
        "any" => Ok(NetworkProtocol::Any),
        "tcp" => Ok(NetworkProtocol::Tcp),
        "udp" => Ok(NetworkProtocol::Udp),
        "icmp" => Ok(NetworkProtocol::Icmp),
        _ => Err(NetworkError::InvalidRequest),
    }
}

fn deterministic_port_mac(port_id: Uuid) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(port_id.as_bytes());
    format!(
        "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use o3k_domain::PolicyAction;
    use o3k_store::DurableStore;

    fn auth(project_id: &str) -> AuthContext {
        AuthContext::new(
            o3k_kernel::Principal::User(o3k_kernel::UserPrincipal::new(
                o3k_kernel::PrincipalId::new_unchecked("test-user"),
                "test-user",
                Some("default".to_string()),
            )),
            o3k_kernel::OwnershipScope::project(
                o3k_kernel::ScopeId::new_unchecked(project_id),
                Some(project_id.to_string()),
                Some("default".to_string()),
            ),
            vec!["admin".to_string()],
            1000,
            5000,
            uuid::Uuid::now_v7().to_string(),
            uuid::Uuid::now_v7().to_string(),
            None,
        )
    }

    fn root(label: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/o3k-network-{label}-{}", std::process::id()))
    }

    #[tokio::test]
    async fn canonical_service_reconstructs_zero_and_multiple_realms()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("canonical-runtime");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_canonical_network_for_project("project-a", "canonical".to_owned())
            .await?;
        let empty = service
            .reconstruct_canonical_network("project-a", network.id)
            .await?;
        assert!(empty.realms.is_empty());

        let realm_a = service
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.0.0.0/24".to_owned(),
                true,
            )
            .await?;
        let realm_b = service
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.0.0.0/24".to_owned(),
                true,
            )
            .await?;
        let realm_c = service
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.1.0.0/24".to_owned(),
                false,
            )
            .await?;
        let pool = service
            .create_canonical_pool_for_project(
                "project-a",
                realm_a.id,
                "10.0.0.0/24".to_owned(),
                Some("10.0.0.1".parse()?),
                "10.0.0.2".parse()?,
                "10.0.0.254".parse()?,
            )
            .await?;
        let endpoint_a = service
            .create_canonical_endpoint_for_project(
                "project-a",
                realm_a.id,
                "10.0.0.10".parse()?,
                "02:00:00:00:00:10".to_owned(),
            )
            .await?;
        let endpoint_b = service
            .create_canonical_endpoint_for_project(
                "project-a",
                realm_b.id,
                "10.0.0.10".parse()?,
                "02:00:00:00:00:11".to_owned(),
            )
            .await?;
        assert_eq!(endpoint_a.fixed_ip, endpoint_b.fixed_ip);
        assert!(matches!(
            service
                .create_canonical_endpoint_for_project(
                    "project-a",
                    realm_a.id,
                    endpoint_a.fixed_ip,
                    "02:00:00:00:00:12".to_owned()
                )
                .await,
            Err(NetworkError::Conflict)
        ));
        assert!(matches!(
            service
                .delete_canonical_realm_for_project("project-a", realm_a.id)
                .await,
            Err(NetworkError::Conflict)
        ));
        drop(service);
        drop(store);

        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        let snapshot = reopened
            .reconstruct_canonical_network("project-a", network.id)
            .await?;
        assert_eq!(snapshot.network.id, network.id);
        assert_eq!(snapshot.realms.len(), 3);
        assert_eq!(snapshot.pools[&realm_a.id], vec![pool]);
        assert_eq!(snapshot.endpoints[&realm_a.id], vec![endpoint_a]);
        reopened
            .delete_canonical_realm_for_project("project-a", realm_c.id)
            .await?;
        assert_eq!(
            reopened
                .reconstruct_canonical_network("project-a", network.id)
                .await?
                .realms
                .len(),
            2
        );
        assert!(matches!(
            reopened
                .delete_canonical_network_for_project("project-a", network.id)
                .await,
            Err(NetworkError::Conflict)
        ));
        drop(reopened);
        drop(reopened_store);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn network_rename_updates_projection_and_reopens_with_new_name()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("rename-restart");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let identity = auth("project-a");
        let network = service
            .create_network(&identity, "before".to_owned())
            .await?;
        let renamed = service
            .update_network(&identity, network.id, Some("after".to_owned()), Some(false))
            .await?;
        assert_eq!(renamed.id, network.id);
        assert_eq!(renamed.name, "after");
        let canonical = store
            .get_canonical_network("project-a", &network.id)
            .await?
            .ok_or("canonical network after rename")?;
        assert!(!canonical.admin_state_up);
        assert_eq!(
            store
                .get_network("project-a", &network.id)
                .await?
                .map(|n| n.name),
            Some("after".to_owned())
        );

        drop(service);
        drop(store);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        let restored = reopened.get_network(&identity, network.id).await?;
        assert_eq!(restored.id, network.id);
        assert_eq!(restored.project_id, "project-a");
        assert_eq!(restored.name, "after");
        let restored_canonical = reopened_store
            .get_canonical_network("project-a", &network.id)
            .await?
            .ok_or("canonical network after restart")?;
        assert!(!restored_canonical.admin_state_up);
        assert_eq!(
            reopened_store
                .get_network("project-a", &network.id)
                .await?
                .map(|n| n.name),
            Some("after".to_owned())
        );
        assert!(
            reopened_store
                .get_network("project-a", &network.id)
                .await?
                .is_some()
        );
        Ok(())
    }

    #[tokio::test]
    async fn canonical_reads_do_not_require_projection_metadata()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("canonical-reads");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_canonical_network_for_project("project-a", "canonical".to_owned())
            .await?;
        let realm = service
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.20.0.0/24".to_owned(),
                false,
            )
            .await?;
        let _pool = service
            .create_canonical_pool_for_project(
                "project-a",
                realm.id,
                "10.20.0.0/24".to_owned(),
                Some("10.20.0.1".parse()?),
                "10.20.0.2".parse()?,
                "10.20.0.254".parse()?,
            )
            .await?;
        let endpoint = service
            .create_canonical_endpoint_for_project(
                "project-a",
                realm.id,
                "10.20.0.10".parse()?,
                "02:00:00:20:00:10".to_owned(),
            )
            .await?;

        let subnet = service
            .get_subnet_for_project("project-a", realm.id)
            .await?;
        assert_eq!(subnet.id, realm.id);
        assert_eq!(subnet.network_id, network.id);
        assert!(subnet.name.is_empty());

        let port = service
            .get_port_for_project("project-a", endpoint.id)
            .await?;
        assert_eq!(port.id, endpoint.id);
        assert_eq!(port.subnet_id, Some(realm.id));
        assert_eq!(port.fixed_ip, endpoint.fixed_ip);
        assert_eq!(port.mac_address, endpoint.mac);
        assert!(port.name.is_empty());

        drop(service);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn authenticated_canonical_entry_points_enforce_scope_and_audit()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("canonical-auth");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let sink = Arc::new(o3k_kernel::MemoryAuditSink::new());
        let service = NetworkService::open(&path, store)
            .await?
            .with_audit_sink(sink.clone());
        let network = service
            .create_canonical_network(&auth("project-a"), "authorized".to_owned())
            .await?;
        let realm = service
            .create_canonical_realm(
                &auth("project-a"),
                network.id,
                "10.30.0.0/24".to_owned(),
                false,
            )
            .await?;
        assert!(matches!(
            service
                .delete_canonical_realm(&auth("project-b"), realm.id)
                .await,
            Err(NetworkError::NotFound)
        ));
        let events = sink.events();
        assert!(events.iter().any(|event| {
            event.action.to_string() == "network:DeleteAddressRealm"
                && event.outcome == AuditOutcome::Denied
                && event
                    .resource_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == realm.id.to_string())
        }));
        Ok(())
    }

    #[tokio::test]
    async fn authenticated_parent_actions_use_canonical_owner_and_audit_outcomes()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("canonical-auth-matrix");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let sink = Arc::new(o3k_kernel::MemoryAuditSink::new());
        let service = NetworkService::open(&path, store)
            .await?
            .with_audit_sink(sink.clone());
        let network = service
            .create_canonical_network(&auth("project-a"), "matrix".to_owned())
            .await?;
        let realm = service
            .create_canonical_realm(
                &auth("project-a"),
                network.id,
                "10.32.0.0/24".to_owned(),
                false,
            )
            .await?;

        assert!(matches!(
            service
                .create_canonical_realm(
                    &auth("project-b"),
                    network.id,
                    "10.33.0.0/24".to_owned(),
                    false,
                )
                .await,
            Err(NetworkError::NotFound)
        ));
        assert!(matches!(
            service
                .create_canonical_pool(
                    &auth("project-b"),
                    realm.id,
                    "10.32.0.0/24".to_owned(),
                    Some("10.32.0.1".parse()?),
                    "10.32.0.2".parse()?,
                    "10.32.0.254".parse()?,
                )
                .await,
            Err(NetworkError::NotFound)
        ));
        assert!(matches!(
            service
                .create_canonical_endpoint(
                    &auth("project-b"),
                    realm.id,
                    "10.32.0.10".parse()?,
                    "02:00:00:32:00:10".to_owned(),
                )
                .await,
            Err(NetworkError::NotFound)
        ));

        let pool = service
            .create_canonical_pool(
                &auth("project-a"),
                realm.id,
                "10.32.0.0/24".to_owned(),
                Some("10.32.0.1".parse()?),
                "10.32.0.2".parse()?,
                "10.32.0.254".parse()?,
            )
            .await?;
        assert_eq!(
            service
                .list_canonical_pools(&auth("project-a"), realm.id)
                .await?
                .len(),
            1
        );
        assert!(matches!(
            service
                .list_canonical_pools(&auth("project-b"), realm.id)
                .await,
            Err(NetworkError::NotFound)
        ));
        let endpoint = service
            .create_canonical_endpoint(
                &auth("project-a"),
                realm.id,
                "10.32.0.10".parse()?,
                "02:00:00:32:00:10".to_owned(),
            )
            .await?;
        assert_eq!(
            service
                .get_canonical_realm(&auth("project-a"), realm.id)
                .await?
                .id,
            realm.id
        );
        assert_eq!(
            service
                .list_canonical_endpoints(&auth("project-a"), realm.id)
                .await?
                .len(),
            1
        );
        assert_eq!(
            service
                .get_canonical_endpoint(&auth("project-a"), endpoint.id)
                .await?
                .id,
            endpoint.id
        );
        assert!(matches!(
            service
                .get_canonical_endpoint(&auth("project-b"), endpoint.id)
                .await,
            Err(NetworkError::NotFound)
        ));
        assert!(matches!(
            service
                .create_canonical_network(&auth("project-a"), "matrix".to_owned())
                .await,
            Err(NetworkError::Conflict)
        ));

        let events = sink.events();
        let denied = events
            .iter()
            .filter(|event| event.outcome == AuditOutcome::Denied)
            .collect::<Vec<_>>();
        assert!(denied.len() >= 3);
        assert!(denied.iter().all(|event| {
            event
                .authorization_decision
                .as_ref()
                .is_some_and(|decision| {
                    decision.reason() == &o3k_kernel::DecisionReason::ScopeMismatch
                })
                && event
                    .owner_scope
                    .as_ref()
                    .is_some_and(|scope| scope.id().as_str() == "project-a")
        }));
        assert!(events.iter().any(|event| {
            event.action.to_string() == "network:CreateNetwork"
                && event.outcome == AuditOutcome::Failed
        }));
        assert!(events.iter().any(|event| {
            event.action.to_string() == "network:CreateEndpoint"
                && event.outcome == AuditOutcome::Succeeded
        }));
        service
            .delete_canonical_endpoint(&auth("project-a"), endpoint.id)
            .await?;
        service
            .delete_canonical_pool(&auth("project-a"), pool.id)
            .await?;
        service
            .delete_canonical_realm(&auth("project-a"), realm.id)
            .await?;
        service
            .delete_canonical_network(&auth("project-a"), network.id)
            .await?;
        let final_events = sink.events();
        assert!(final_events.iter().any(|event| {
            event.action.to_string() == "network:DeleteNetwork"
                && event.outcome == AuditOutcome::Succeeded
        }));
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_file(sqlite_path);
        Ok(())
    }

    #[tokio::test]
    async fn independent_services_preserve_canonical_endpoint_and_realm_races()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("canonical-races");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store_a = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let store_b = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service_a = NetworkService::open(&path, store_a).await?;
        let service_b = NetworkService::open(&path, store_b).await?;
        let network = service_a
            .create_canonical_network_for_project("project-a", "races".to_owned())
            .await?;
        let realm = service_a
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.31.0.0/24".to_owned(),
                false,
            )
            .await?;
        let (left, right) = tokio::join!(
            service_a.create_canonical_endpoint_for_project(
                "project-a",
                realm.id,
                "10.31.0.10".parse()?,
                "02:00:00:00:31:10".to_owned(),
            ),
            service_b.create_canonical_endpoint_for_project(
                "project-a",
                realm.id,
                "10.31.0.10".parse()?,
                "02:00:00:00:31:11".to_owned(),
            )
        );
        assert_eq!(left.is_ok() as u8 + right.is_ok() as u8, 1);

        let delete = service_a.delete_canonical_realm_for_project("project-a", realm.id);
        let create = service_b.create_canonical_endpoint_for_project(
            "project-a",
            realm.id,
            "10.31.0.11".parse()?,
            "02:00:00:00:31:12".to_owned(),
        );
        let (delete_result, create_result) = tokio::join!(delete, create);
        if delete_result.is_ok() {
            assert!(create_result.is_err());
            assert!(
                service_a
                    .reconstruct_canonical_network("project-a", network.id)
                    .await?
                    .realms
                    .is_empty()
            );
        } else {
            assert!(create_result.is_ok());
            assert!(
                service_a
                    .reconstruct_canonical_network("project-a", network.id)
                    .await?
                    .realms
                    .iter()
                    .any(|value| value.id == realm.id)
            );
        }
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_file(sqlite_path);
        Ok(())
    }

    #[tokio::test]
    async fn independent_services_preserve_network_realm_races()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("canonical-network-realm-races");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store_a = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let store_b = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service_a = NetworkService::open(&path, store_a).await?;
        let service_b = NetworkService::open(&path, store_b).await?;
        let network = service_a
            .create_canonical_network_for_project("project-a", "parent-race".to_owned())
            .await?;

        let (first, second) = tokio::join!(
            service_a.create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.34.0.0/24".to_owned(),
                false,
            ),
            service_b.create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.35.0.0/24".to_owned(),
                false,
            )
        );
        let created = [first, second]
            .into_iter()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert_eq!(created.len(), 2);
        let realms = service_a
            .reconstruct_canonical_network("project-a", network.id)
            .await?
            .realms;
        assert_eq!(realms.len(), 2);
        assert!(realms.iter().all(|realm| realm.network_id == network.id));

        let (delete, create) = tokio::join!(
            service_a.delete_canonical_network_for_project("project-a", network.id),
            service_b.create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.36.0.0/24".to_owned(),
                false,
            )
        );
        assert!(delete.is_err());
        assert!(create.is_ok());
        let snapshot = service_a
            .reconstruct_canonical_network("project-a", network.id)
            .await?;
        assert!(
            snapshot
                .realms
                .iter()
                .all(|realm| realm.network_id == snapshot.network.id)
        );
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_file(sqlite_path);
        Ok(())
    }

    #[tokio::test]
    async fn realm_deletion_is_fenced_when_provider_binding_remains()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("realm-deletion-fence");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_canonical_network_for_project("project-a", "fenced".to_owned())
            .await?;
        let realm = service
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.30.0.0/24".to_owned(),
                false,
            )
            .await?;
        store
            .insert_canonical_realm_binding(&o3k_store::CanonicalRealmBindingRecord {
                fabric_domain_id: "fabric-a".to_owned(),
                realm_id: realm.id,
                provider_kind: "geneve".to_owned(),
                provider_segment_id: 300,
                binding_generation: 1,
                state: "active".to_owned(),
            })
            .await?;

        assert!(matches!(
            service
                .delete_canonical_realm_for_project("project-a", realm.id)
                .await,
            Err(NetworkError::Conflict)
        ));
        let deleting = store
            .get_canonical_realm("project-a", &realm.id)
            .await?
            .ok_or("realm disappeared during fenced deletion")?;
        assert_eq!(deleting.state, "deleting");
        assert!(matches!(
            service
                .create_canonical_endpoint_for_project(
                    "project-a",
                    realm.id,
                    "10.30.0.10".parse()?,
                    "02:00:00:30:00:10".to_owned(),
                )
                .await,
            Err(NetworkError::InvalidRequest) | Err(NetworkError::Conflict)
        ));
        assert!(matches!(
            service
                .delete_canonical_realm_for_project("project-a", realm.id)
                .await,
            Err(NetworkError::Conflict)
        ));
        assert!(
            service
                .get_canonical_network_for_project("project-a", network.id)
                .await
                .is_ok()
        );
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_file(sqlite_path);
        Ok(())
    }

    #[tokio::test]
    async fn realm_cleanup_unknown_outcome_replays_and_finalizes_after_observation()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("realm-cleanup-recovery");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_canonical_network_for_project("project-a", "recovery".to_owned())
            .await?;
        let realm = service
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.31.0.0/24".to_owned(),
                false,
            )
            .await?;
        let binding = o3k_store::CanonicalRealmBindingRecord {
            fabric_domain_id: "fabric-a".to_owned(),
            realm_id: realm.id,
            provider_kind: "geneve".to_owned(),
            provider_segment_id: 301,
            binding_generation: 1,
            state: "active".to_owned(),
        };
        store.insert_canonical_realm_binding(&binding).await?;

        assert!(matches!(
            service
                .delete_canonical_realm_for_project("project-a", realm.id)
                .await,
            Err(NetworkError::Conflict)
        ));
        let first = service
            .begin_canonical_realm_deletion_for_project("project-a", realm.id)
            .await?;
        let operation_id = match first {
            RealmCleanupProgress::AwaitingObservation { operation_id, .. } => operation_id,
            _ => return Err("unexpected replay progress".into()),
        };
        let unknown = service
            .observe_canonical_realm_cleanup_for_project(
                "project-a",
                realm.id,
                vec![RealmCleanupObservation::Unknown {
                    binding: binding.clone(),
                    reason: "provider response lost".to_owned(),
                }],
            )
            .await?;
        assert!(matches!(
            unknown,
            RealmCleanupProgress::AwaitingObservation { .. }
        ));
        assert_eq!(
            store.get_canonical_operation(operation_id).await?.state,
            o3k_store::OperationState::UnknownOutcome
        );

        drop(service);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        let replay = reopened
            .begin_canonical_realm_deletion_for_project("project-a", realm.id)
            .await?;
        assert!(matches!(
            replay,
            RealmCleanupProgress::AwaitingObservation { operation_id: id, .. } if id == operation_id
        ));
        let present = reopened
            .observe_canonical_realm_cleanup_for_project(
                "project-a",
                realm.id,
                vec![RealmCleanupObservation::Present(binding.clone())],
            )
            .await?;
        assert!(matches!(
            present,
            RealmCleanupProgress::AwaitingObservation { .. }
        ));
        let removed = reopened
            .observe_canonical_realm_cleanup_for_project(
                "project-a",
                realm.id,
                vec![RealmCleanupObservation::Absent(binding)],
            )
            .await?;
        assert_eq!(removed, RealmCleanupProgress::Removed { operation_id });
        assert!(
            reopened
                .get_canonical_network_for_project("project-a", network.id)
                .await
                .is_ok()
        );
        assert!(matches!(
            reopened
                .reconstruct_canonical_network("project-a", network.id)
                .await,
            Ok(snapshot) if snapshot.realms.is_empty()
        ));
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_file(sqlite_path);
        Ok(())
    }

    #[tokio::test]
    async fn allocation_is_deterministic_collision_safe_and_restartable()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("allocation");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_network(&auth("project-a"), "flat".to_owned())
            .await?;
        let subnet = service
            .create_subnet(
                &auth("project-a"),
                network.id,
                "lab".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let first = service
            .create_port(&auth("project-a"), network.id, "one".to_owned())
            .await?;
        let second = service
            .create_port(&auth("project-a"), network.id, "two".to_owned())
            .await?;
        assert_ne!(first.fixed_ip, second.fixed_ip);
        assert_ne!(first.mac_address, second.mac_address);
        assert_eq!(first.mac_address, deterministic_port_mac(first.id));
        assert_eq!(first.fixed_ip, subnet.allocation_start);
        drop(service);
        drop(store);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        assert_eq!(
            reopened.get_port(&auth("project-a"), first.id).await?,
            first
        );
        reopened.delete_port(&auth("project-a"), first.id).await?;
        let replacement = reopened
            .create_port(&auth("project-a"), network.id, "replacement".to_owned())
            .await?;
        assert_eq!(replacement.fixed_ip, first.fixed_ip);
        assert!(!fs::read_dir(&path)?.flatten().any(|entry| {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            name.contains("metadata.tmp-") || name.contains("metadata.json")
        }));
        drop(reopened);
        drop(reopened_store);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn legacy_metadata_file_is_imported_once_and_never_read_again()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("legacy-import");
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path)?;
        let network_id = Uuid::now_v7();
        let subnet_id = Uuid::now_v7();
        let port_with_mac = Uuid::now_v7();
        let port_without_mac = Uuid::now_v7();
        let port_without_subnet = Uuid::now_v7();
        let legacy = serde_json::json!({
            "networks": [{
                "id": network_id,
                "name": "flat",
                "project_id": "project-a",
                "status": "ACTIVE"
            }],
            "subnets": [{
                "id": subnet_id,
                "network_id": network_id,
                "name": "lab",
                "project_id": "project-a",
                "cidr": "192.0.2.0/29",
                "gateway_ip": "192.0.2.1",
                "allocation_start": "192.0.2.2",
                "allocation_end": "192.0.2.14"
            }],
            "ports": [
                {
                    "id": port_with_mac,
                    "network_id": network_id,
                    "subnet_id": subnet_id,
                    "project_id": "project-a",
                    "name": "with-mac",
                    "mac_address": "02:00:00:00:00:99",
                    "fixed_ip": "192.0.2.2",
                    "status": "ACTIVE"
                },
                {
                    "id": port_without_mac,
                    "network_id": network_id,
                    "subnet_id": subnet_id,
                    "project_id": "project-a",
                    "name": "no-mac",
                    "fixed_ip": "192.0.2.3",
                    "status": "ACTIVE"
                },
                {
                    "id": port_without_subnet,
                    "network_id": network_id,
                    "project_id": "project-a",
                    "name": "no-subnet",
                    "mac_address": "02:00:00:00:00:98",
                    "fixed_ip": "192.0.2.4",
                    "status": "ACTIVE"
                }
            ]
        });
        fs::write(path.join("metadata.json"), serde_json::to_vec(&legacy)?)?;
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        assert_eq!(service.list_networks(&auth("project-a")).await?.len(), 1);
        assert_eq!(service.list_subnets(&auth("project-a")).await?.len(), 1);
        assert_eq!(service.list_ports(&auth("project-a")).await?.len(), 3);
        let network = service.get_network(&auth("project-a"), network_id).await?;
        assert_eq!(network.id, network_id);
        let subnet = service.get_subnet(&auth("project-a"), subnet_id).await?;
        assert_eq!(subnet.id, subnet_id);
        let first = service.get_port(&auth("project-a"), port_with_mac).await?;
        assert_eq!(first.mac_address, "02:00:00:00:00:99");
        assert_eq!(first.subnet_id, Some(subnet_id));
        let migrated_mac = service
            .get_port(&auth("project-a"), port_without_mac)
            .await?;
        assert_eq!(
            migrated_mac.mac_address,
            deterministic_port_mac(port_without_mac)
        );
        assert_eq!(migrated_mac.subnet_id, Some(subnet_id));
        let migrated_subnet = service
            .get_port(&auth("project-a"), port_without_subnet)
            .await?;
        assert_eq!(migrated_subnet.subnet_id, Some(subnet_id));
        assert_eq!(migrated_subnet.mac_address, "02:00:00:00:00:98");
        assert!(!path.join("metadata.json").exists());
        assert!(path.join("metadata.json.imported").exists());
        let second = NetworkService::open(&path, store).await?;
        assert_eq!(second.list_networks(&auth("project-a")).await?.len(), 1);
        assert_eq!(second.list_subnets(&auth("project-a")).await?.len(), 1);
        assert_eq!(second.list_ports(&auth("project-a")).await?.len(), 3);
        drop(second);
        fs::remove_dir_all(path)?;

        let corrupt_path = root("legacy-import-corrupt");
        let _ = fs::remove_dir_all(&corrupt_path);
        fs::create_dir_all(&corrupt_path)?;
        fs::write(corrupt_path.join("metadata.json"), b"not-json")?;
        let corrupt_store = Arc::new(o3k_store::testkit::open_memory().await?);
        assert!(matches!(
            NetworkService::open(&corrupt_path, corrupt_store).await,
            Err(NetworkError::CorruptMetadata(_))
        ));
        assert!(corrupt_path.join("metadata.json").exists());
        fs::remove_dir_all(corrupt_path)?;

        let duplicate_path = root("legacy-import-duplicate-mac");
        let _ = fs::remove_dir_all(&duplicate_path);
        fs::create_dir_all(&duplicate_path)?;
        let duplicated = serde_json::json!({
            "networks": [],
            "subnets": [],
            "ports": [
                {
                    "id": Uuid::now_v7(),
                    "network_id": Uuid::now_v7(),
                    "project_id": "project-a",
                    "name": "one",
                    "mac_address": "02:00:00:00:00:01",
                    "fixed_ip": "192.0.2.2",
                    "status": "ACTIVE"
                },
                {
                    "id": Uuid::now_v7(),
                    "network_id": Uuid::now_v7(),
                    "project_id": "project-a",
                    "name": "two",
                    "mac_address": "02:00:00:00:00:01",
                    "fixed_ip": "192.0.2.3",
                    "status": "ACTIVE"
                }
            ]
        });
        fs::write(
            duplicate_path.join("metadata.json"),
            serde_json::to_vec(&duplicated)?,
        )?;
        let duplicate_store = Arc::new(o3k_store::testkit::open_memory().await?);
        assert!(matches!(
            NetworkService::open(&duplicate_path, duplicate_store).await,
            Err(NetworkError::Conflict)
        ));
        assert!(duplicate_path.join("metadata.json").exists());
        fs::remove_dir_all(duplicate_path)?;
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_port_creation_never_allocates_duplicate_ips_or_macs()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("o3k-network-concurrent-{}", Uuid::now_v7()));
        let sqlite_path = path.with_extension("sqlite");
        fs::create_dir_all(&path)?;
        let setup_store = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let setup = NetworkService::open(&path, setup_store.clone()).await?;
        let network = setup
            .create_network(&auth("project-a"), "flat".to_owned())
            .await?;
        let subnet = setup
            .create_subnet(
                &auth("project-a"),
                network.id,
                "lab".to_owned(),
                "192.0.2.0/28".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(subnet.cidr, "192.0.2.0/28");
        drop(setup);
        drop(setup_store);

        let store_a = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let store_b = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let service_a = NetworkService::open(&path, store_a).await?;
        let service_b = NetworkService::open(&path, store_b).await?;
        let mut handles = Vec::new();
        for index in 0..12 {
            let service = if index % 2 == 0 {
                service_a.clone()
            } else {
                service_b.clone()
            };
            let network_id = network.id;
            handles.push(tokio::spawn(async move {
                service
                    .create_port(&auth("project-a"), network_id, format!("port-{index}"))
                    .await
            }));
        }
        let mut ports = Vec::new();
        for handle in handles {
            match handle.await? {
                Ok(port) => ports.push(port),
                Err(NetworkError::PoolExhausted) => {}
                Err(error) => return Err(error.into()),
            }
        }
        assert_eq!(ports.len(), 12);
        let ips: HashSet<Ipv4Addr> = ports.iter().map(|port| port.fixed_ip).collect();
        let macs: HashSet<String> = ports
            .iter()
            .map(|port| port.mac_address.to_ascii_lowercase())
            .collect();
        assert_eq!(ports.len(), ips.len());
        assert_eq!(ports.len(), macs.len());
        drop(service_a);
        drop(service_b);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{}-wal", sqlite_path.display()));
        let _ = fs::remove_file(format!("{}-shm", sqlite_path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_explicit_fixed_ip_creation_has_one_winner()
    -> Result<(), Box<dyn std::error::Error>> {
        let path =
            std::env::temp_dir().join(format!("o3k-network-explicit-race-{}", Uuid::now_v7()));
        let sqlite_path = path.with_extension("sqlite");
        fs::create_dir_all(&path)?;
        let setup_store = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let setup = NetworkService::open(&path, setup_store.clone()).await?;
        let network = setup
            .create_network(&auth("project-a"), "flat".to_owned())
            .await?;
        let subnet = setup
            .create_subnet(
                &auth("project-a"),
                network.id,
                "lab".to_owned(),
                "192.0.2.0/28".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        assert!(matches!(
            setup
                .create_port_with_fixed_ip(
                    &auth("project-a"),
                    network.id,
                    "outside-pool".to_owned(),
                    Some((subnet.id, Some(Ipv4Addr::new(203, 0, 113, 5)))),
                )
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        assert!(matches!(
            setup
                .create_port_with_fixed_ip(
                    &auth("project-a"),
                    network.id,
                    "o3k-server:project-a:spoof".to_owned(),
                    Some((subnet.id, None)),
                )
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        let server_port = setup
            .create_port_for_project(
                "project-a",
                network.id,
                "o3k-server:project-a:owned".to_owned(),
            )
            .await?;
        assert!(matches!(
            setup
                .update_port_name_for_project("project-a", server_port.id, "renamed".to_owned(),)
                .await,
            Err(NetworkError::Conflict)
        ));
        setup
            .delete_port_for_project("project-a", server_port.id)
            .await?;
        drop(setup);
        drop(setup_store);

        let service_a = NetworkService::open(
            &path,
            Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?),
        )
        .await?;
        let service_b = NetworkService::open(
            &path,
            Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?),
        )
        .await?;
        let fixed_ip = Ipv4Addr::new(192, 0, 2, 5);
        let first = tokio::spawn({
            let service = service_a.clone();
            async move {
                service
                    .create_port_with_fixed_ip(
                        &auth("project-a"),
                        network.id,
                        "first".to_owned(),
                        Some((subnet.id, Some(fixed_ip))),
                    )
                    .await
            }
        });
        let second = tokio::spawn({
            let service = service_b.clone();
            async move {
                service
                    .create_port_with_fixed_ip(
                        &auth("project-a"),
                        network.id,
                        "second".to_owned(),
                        Some((subnet.id, Some(fixed_ip))),
                    )
                    .await
            }
        });
        let outcomes = [first.await?, second.await?];
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(result, Err(NetworkError::Conflict)))
                .count(),
            1
        );
        drop(service_a);
        drop(service_b);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{}-wal", sqlite_path.display()));
        let _ = fs::remove_file(format!("{}-shm", sqlite_path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_cross_instance_writers_conflict_deterministically_without_duplicates()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("o3k-network-multiwriter-{}", Uuid::now_v7()));
        let sqlite_path = path.with_extension("sqlite");
        fs::create_dir_all(&path)?;
        let store_a = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let store_b = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let service_a = NetworkService::open(&path, store_a).await?;
        let service_b = NetworkService::open(&path, store_b).await?;
        let auth_a = auth("project-a");
        let auth_b = auth("project-a");
        // Two writers create a network with the same name: exactly one wins.
        let (first, second) = tokio::join!(
            service_a.create_network(&auth_a, "flat".to_owned()),
            service_b.create_network(&auth_b, "flat".to_owned()),
        );
        assert_eq!([&first, &second].iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            [&first, &second]
                .iter()
                .filter(|r| matches!(r, Err(NetworkError::Conflict)))
                .count(),
            1
        );
        let network_id = first
            .or(second)
            .map_err(|_| "expected one network create to succeed")?
            .id;
        // Same cidr on the same network: exactly one subnet survives.
        let (subnet_first, subnet_second) = tokio::join!(
            service_a.create_subnet(
                &auth_a,
                network_id,
                "lab".to_owned(),
                "192.0.2.0/27".to_owned(),
                None,
                None,
                None,
            ),
            service_b.create_subnet(
                &auth_b,
                network_id,
                "lab".to_owned(),
                "192.0.2.0/27".to_owned(),
                None,
                None,
                None,
            ),
        );
        assert_eq!(
            [&subnet_first, &subnet_second]
                .iter()
                .filter(|r| r.is_ok())
                .count(),
            1
        );
        assert_eq!(
            [&subnet_first, &subnet_second]
                .iter()
                .filter(|r| matches!(r, Err(NetworkError::Conflict)))
                .count(),
            1
        );
        // 40 concurrent port creates across two writers over a 29-address
        // pool: every allocation is distinct and the pool is exhausted
        // deterministically.
        let mut handles = Vec::new();
        for index in 0..40 {
            let service = if index % 2 == 0 {
                service_a.clone()
            } else {
                service_b.clone()
            };
            handles.push(tokio::spawn(async move {
                service
                    .create_port(&auth("project-a"), network_id, format!("port-{index}"))
                    .await
            }));
        }
        let mut ports = Vec::new();
        let mut exhausted = 0;
        for handle in handles {
            match handle.await? {
                Ok(port) => ports.push(port),
                Err(NetworkError::PoolExhausted) => exhausted += 1,
                Err(error) => return Err(error.into()),
            }
        }
        assert_eq!(ports.len(), 29);
        assert_eq!(exhausted, 11);
        let ips: HashSet<Ipv4Addr> = ports.iter().map(|port| port.fixed_ip).collect();
        let macs: HashSet<String> = ports
            .iter()
            .map(|port| port.mac_address.to_ascii_lowercase())
            .collect();
        assert_eq!(ports.len(), ips.len());
        assert_eq!(ports.len(), macs.len());
        // Concurrent deletion of one port: exactly one writer wins.
        let (delete_first, delete_second) = tokio::join!(
            service_a.delete_port(&auth_a, ports[0].id),
            service_b.delete_port(&auth_b, ports[0].id),
        );
        assert_eq!(
            [&delete_first, &delete_second]
                .iter()
                .filter(|r| r.is_ok())
                .count(),
            1
        );
        assert_eq!(
            [&delete_first, &delete_second]
                .iter()
                .filter(|r| matches!(r, Err(NetworkError::NotFound)))
                .count(),
            1
        );
        drop(service_a);
        drop(service_b);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{}-wal", sqlite_path.display()));
        let _ = fs::remove_file(format!("{}-shm", sqlite_path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn binding_state_strings_round_trip_through_canonical_parsing() {
        for state in [
            PortBindingState::Binding,
            PortBindingState::Bound,
            PortBindingState::Down,
            PortBindingState::Error,
        ] {
            assert_eq!(PortBindingState::parse(state.as_str()), Some(state));
        }
        assert_eq!(PortBindingState::parse("unbound"), None);
        assert_eq!(PortBindingState::parse("banana"), None);
        assert_eq!(PortBindingState::parse(""), None);
    }

    #[tokio::test]
    async fn binding_intent_and_observation_projection_are_durable()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("binding");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_network(&auth("project-a"), "flat".to_owned())
            .await?;
        let _subnet = service
            .create_subnet(
                &auth("project-a"),
                network.id,
                "lab".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = service
            .create_port(&auth("project-a"), network.id, "one".to_owned())
            .await?;
        let intended = service
            .record_binding_intent("project-a", port.id, "compute-1")
            .await?;
        assert_eq!(intended.binding_host.as_deref(), Some("compute-1"));
        assert_eq!(intended.binding_state.as_deref(), Some("binding"));
        let observed = service
            .project_binding_observation("project-a", port.id, "compute-1", "bound")
            .await?;
        assert_eq!(observed.binding_host.as_deref(), Some("compute-1"));
        assert_eq!(observed.binding_state.as_deref(), Some("bound"));
        assert!(matches!(
            service
                .project_binding_observation("project-a", port.id, "compute-1", "banana")
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        // An idempotent dispatch replay of the same create must not downgrade
        // the completed `bound` observation back to `binding`.
        let replayed = service
            .record_binding_intent("project-a", port.id, "compute-1")
            .await?;
        assert_eq!(replayed.binding_state.as_deref(), Some("bound"));
        // A fresh dispatch after an observed failure resets to `binding`.
        let down = service
            .project_binding_observation("project-a", port.id, "compute-1", "down")
            .await?;
        assert_eq!(down.binding_state.as_deref(), Some("down"));
        let retried = service
            .record_binding_intent("project-a", port.id, "compute-1")
            .await?;
        assert_eq!(retried.binding_state.as_deref(), Some("binding"));
        assert!(matches!(
            service
                .project_binding_observation("project-a", port.id, "compute-2", "bound")
                .await,
            Err(NetworkError::Conflict)
        ));
        assert!(matches!(
            service
                .record_binding_intent("project-a", port.id, "compute-2")
                .await,
            Err(NetworkError::Conflict)
        ));
        assert!(matches!(
            service
                .project_binding_observation("project-a", Uuid::now_v7(), "compute-1", "bound")
                .await,
            Err(NetworkError::NotFound)
        ));
        assert!(matches!(
            service
                .record_binding_intent("project-a", port.id, "  ")
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        let final_observed = service
            .project_binding_observation("project-a", port.id, "compute-1", "bound")
            .await?;
        assert_eq!(final_observed.binding_state.as_deref(), Some("bound"));
        drop(service);
        drop(store);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        let restored = reopened.get_port(&auth("project-a"), port.id).await?;
        assert_eq!(restored.binding_host.as_deref(), Some("compute-1"));
        assert_eq!(restored.binding_state.as_deref(), Some("bound"));
        drop(reopened);
        drop(reopened_store);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn delete_cleanup_and_ip_reuse_after_restart() -> Result<(), Box<dyn std::error::Error>> {
        let path = root("delete-reuse");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_network(&auth("project-a"), "flat".to_owned())
            .await?;
        let subnet = service
            .create_subnet(
                &auth("project-a"),
                network.id,
                "lab".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = service
            .create_port(&auth("project-a"), network.id, "one".to_owned())
            .await?;
        service.delete_port(&auth("project-a"), port.id).await?;
        assert!(matches!(
            service.get_port(&auth("project-a"), port.id).await,
            Err(NetworkError::NotFound)
        ));
        drop(service);
        drop(store);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        let replacement = reopened
            .create_port(&auth("project-a"), network.id, "replacement".to_owned())
            .await?;
        assert_eq!(replacement.fixed_ip, port.fixed_ip);
        assert_ne!(replacement.mac_address, port.mac_address);
        reopened
            .delete_port(&auth("project-a"), replacement.id)
            .await?;
        reopened
            .delete_subnet(&auth("project-a"), subnet.id)
            .await?;
        reopened
            .delete_network(&auth("project-a"), network.id)
            .await?;
        assert!(matches!(
            reopened.get_network(&auth("project-a"), network.id).await,
            Err(NetworkError::NotFound)
        ));
        drop(reopened);
        drop(reopened_store);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn create_outcome_projection_and_unbind_are_durable_and_idempotent()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("create-outcome");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_network(&auth("project-a"), "flat".to_owned())
            .await?;
        let _subnet = service
            .create_subnet(
                &auth("project-a"),
                network.id,
                "lab".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = service
            .create_port(&auth("project-a"), network.id, "one".to_owned())
            .await?;
        // Without a recorded intent the projection is rejected.
        assert!(matches!(
            service
                .project_create_outcome("project-a", port.id, PortBindingState::Bound)
                .await,
            Err(NetworkError::Conflict)
        ));
        service
            .record_binding_intent("project-a", port.id, "compute-1")
            .await?;
        // The observed state is set on the host recorded by the intent.
        let bound = service
            .project_create_outcome("project-a", port.id, PortBindingState::Bound)
            .await?;
        assert_eq!(bound.binding_host.as_deref(), Some("compute-1"));
        assert_eq!(bound.binding_state.as_deref(), Some("bound"));
        // A failed outcome after a fresh intent projects `error`.
        service
            .project_binding_observation("project-a", port.id, "compute-1", "down")
            .await?;
        service
            .record_binding_intent("project-a", port.id, "compute-1")
            .await?;
        let errored = service
            .project_create_outcome("project-a", port.id, PortBindingState::Error)
            .await?;
        assert_eq!(errored.binding_state.as_deref(), Some("error"));
        // Only terminal create outcomes are projectable.
        assert!(matches!(
            service
                .project_create_outcome("project-a", port.id, PortBindingState::Binding)
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        assert!(matches!(
            service
                .project_create_outcome("project-a", Uuid::now_v7(), PortBindingState::Bound)
                .await,
            Err(NetworkError::NotFound)
        ));
        // Unbind clears the binding idempotently and is durable.
        let unbound = service.unbind_port("project-a", port.id).await?;
        assert_eq!(unbound.binding_host, None);
        assert_eq!(unbound.binding_state, None);
        let again = service.unbind_port("project-a", port.id).await?;
        assert_eq!(again.binding_host, None);
        assert!(matches!(
            service.unbind_port("project-a", Uuid::now_v7()).await,
            Err(NetworkError::NotFound)
        ));
        drop(service);
        drop(store);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        let restored = reopened.get_port(&auth("project-a"), port.id).await?;
        assert_eq!(restored.binding_host, None);
        assert_eq!(restored.binding_state, None);
        drop(reopened);
        drop(reopened_store);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn invalid_cidr_exhaustion_and_project_isolation_are_enforced()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("validation");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store).await?;
        let network = service
            .create_network(&auth("project-a"), "flat".to_owned())
            .await?;
        assert!(matches!(
            service
                .create_subnet(
                    &auth("project-a"),
                    network.id,
                    "bad".to_owned(),
                    "192.0.2.1/31".to_owned(),
                    None,
                    None,
                    None
                )
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        let _ = service
            .create_subnet(
                &auth("project-a"),
                network.id,
                "tiny".to_owned(),
                "192.0.2.0/30".to_owned(),
                None,
                Some(Ipv4Addr::new(192, 0, 2, 2)),
                Some(Ipv4Addr::new(192, 0, 2, 2)),
            )
            .await?;
        let _ = service
            .create_port(&auth("project-a"), network.id, "one".to_owned())
            .await?;
        assert!(matches!(
            service
                .create_port(&auth("project-a"), network.id, "two".to_owned())
                .await,
            Err(NetworkError::PoolExhausted)
        ));
        assert!(matches!(
            service
                .create_subnet(
                    &auth("project-a"),
                    network.id,
                    "gateway-overlap".to_owned(),
                    "198.51.100.0/29".to_owned(),
                    Some(Ipv4Addr::new(198, 51, 100, 3)),
                    Some(Ipv4Addr::new(198, 51, 100, 2)),
                    Some(Ipv4Addr::new(198, 51, 100, 4)),
                )
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        assert!(matches!(
            service.get_network(&auth("project-b"), network.id).await,
            Err(NetworkError::NotFound)
        ));
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn network_quota_enforcement_and_isolation() -> Result<(), Box<dyn std::error::Error>> {
        use o3k_store::QuotaRepository;

        let path = root("network-quota-isolation");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store.clone()).await?;

        let scope_a = OwnershipScope::project(ScopeId::new_unchecked("proj-a"), None, None);

        // Limit proj-a to 1 network
        store
            .set_limit(
                &scope_a,
                &LimitKey::network_networks(),
                LimitValue::Maximum(1),
            )
            .await?;

        let auth_a = auth("proj-a");
        let auth_b = auth("proj-b");

        // 1. First network for proj-a succeeds
        let net1 = service.create_network(&auth_a, "net-1".to_owned()).await?;
        assert_eq!(net1.name, "net-1");

        // 2. Second network for proj-a fails with QuotaExceeded
        let res2 = service.create_network(&auth_a, "net-2".to_owned()).await;
        assert!(matches!(res2, Err(NetworkError::QuotaExceeded { .. })));

        // 3. Proj-b can create network (isolation)
        let net_b = service.create_network(&auth_b, "net-b".to_owned()).await?;
        assert_eq!(net_b.name, "net-b");

        // 4. Deleting net1 frees quota for proj-a
        service.delete_network(&auth_a, net1.id).await?;

        let net2 = service.create_network(&auth_a, "net-2".to_owned()).await?;
        assert_eq!(net2.name, "net-2");

        let _ = fs::remove_dir_all(&path);
        Ok(())
    }

    #[tokio::test]
    async fn network_subnet_and_port_quota_enforcement() -> Result<(), Box<dyn std::error::Error>> {
        use o3k_store::QuotaRepository;

        let path = root("network-subnets-ports-quota");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store.clone()).await?;

        let scope_a = OwnershipScope::project(ScopeId::new_unchecked("proj-sub-port"), None, None);
        let auth_a = auth("proj-sub-port");

        // 1. Set subnet limit = 1 and port limit = 1
        store
            .set_limit(
                &scope_a,
                &LimitKey::network_subnets(),
                LimitValue::Maximum(1),
            )
            .await?;
        store
            .set_limit(&scope_a, &LimitKey::network_ports(), LimitValue::Maximum(1))
            .await?;

        let net = service
            .create_network(&auth_a, "net-main".to_owned())
            .await?;

        // 2. Subnet creation: 1st succeeds, 2nd fails
        let sub1 = service
            .create_subnet(
                &auth_a,
                net.id,
                "sub-1".to_owned(),
                "10.0.0.0/24".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(sub1.network_id, net.id);

        let sub2_res = service
            .create_subnet(
                &auth_a,
                net.id,
                "sub-2".to_owned(),
                "10.0.1.0/24".to_owned(),
                None,
                None,
                None,
            )
            .await;
        assert!(matches!(sub2_res, Err(NetworkError::QuotaExceeded { .. })));

        // 3. Port creation: 1st succeeds, 2nd fails
        let port1 = service
            .create_port(&auth_a, net.id, "port-1".to_owned())
            .await?;
        assert_eq!(port1.network_id, net.id);

        let port2_res = service
            .create_port(&auth_a, net.id, "port-2".to_owned())
            .await;
        assert!(matches!(port2_res, Err(NetworkError::QuotaExceeded { .. })));

        // 4. Delete port1 and subnet1 -> frees quota
        service.delete_port(&auth_a, port1.id).await?;
        service.delete_subnet(&auth_a, sub1.id).await?;

        // 5. Subsequent creates succeed
        let sub2 = service
            .create_subnet(
                &auth_a,
                net.id,
                "sub-2".to_owned(),
                "10.0.1.0/24".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(sub2.network_id, net.id);

        let port2 = service
            .create_port(&auth_a, net.id, "port-2".to_owned())
            .await?;
        assert_eq!(port2.network_id, net.id);

        let _ = fs::remove_dir_all(&path);
        Ok(())
    }

    #[tokio::test]
    async fn policy_intent_is_durable_generation_fenced_and_compiled()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("policy-intent");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let project = "project-a";
        let network = service
            .create_network(&auth(project), "policy-net".to_owned())
            .await?;
        service
            .create_subnet(
                &auth(project),
                network.id,
                "policy-subnet".to_owned(),
                "10.20.0.0/24".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = service
            .create_port(&auth(project), network.id, "policy-port".to_owned())
            .await?;
        let policy_id = Uuid::now_v7();
        let policy = PolicyIntent {
            id: policy_id,
            endpoint_id: port.id,
            direction: PolicyDirection::Ingress,
            protocol: NetworkProtocol::Tcp,
            ports: Some(o3k_domain::PortRange {
                start: 8080,
                end: 8080,
            }),
            source: Some(
                o3k_domain::Ipv4Prefix::new("198.51.100.0".parse()?, 24)
                    .ok_or("invalid source prefix")?,
            ),
            destination: None,
            action: PolicyAction::Deny,
        };
        service
            .upsert_policy_for_project(project, network.id, policy.clone())
            .await?;
        assert_eq!(
            service
                .list_policies_for_project(project, network.id)
                .await?,
            vec![policy.clone()]
        );
        let canonical = store.list_canonical_policies(project, &network.id).await?;
        assert_eq!(canonical.len(), 1);
        assert_eq!(canonical[0].id, policy_id);
        let legacy = store.get_network_intent(project, &network.id).await?;
        assert!(!legacy.is_some_and(|record| record.payload.contains(&policy_id.to_string())));

        let compiled = compile_attachment_plan(AttachmentPlanInput {
            endpoint_id: port.id,
            realm_id: network.id,
            project_id: project,
            mac: &port.mac_address,
            fixed_ip: port.fixed_ip,
            subnet_cidr: "10.20.0.0/24",
            node_id: "network-agent-1",
            operation_id: Uuid::now_v7(),
            deadline_unix_ms: 1,
            public_address: None,
            external_realm_id: None,
            policies: vec![policy.clone()],
        })?;
        assert!(compiled.intents.iter().any(|intent| matches!(
            intent,
            NetworkPlanIntent::Policy(value) if value == &policy
        )));

        service
            .delete_policy_for_project(project, network.id, policy_id)
            .await?;
        assert!(
            service
                .list_policies_for_project(project, network.id)
                .await?
                .is_empty()
        );
        assert!(matches!(
            service
                .delete_policy_for_project("other-project", network.id, policy_id)
                .await,
            Err(NetworkError::NotFound)
        ));
        let _ = fs::remove_dir_all(&path);
        Ok(())
    }

    #[test]
    fn attachment_plan_can_carry_operator_owned_routed_egress() -> Result<(), NetworkPlanError> {
        let endpoint_id = Uuid::from_u128(11);
        let external_realm_id = Uuid::from_u128(12);
        let plan = compile_attachment_plan(AttachmentPlanInput {
            endpoint_id,
            realm_id: Uuid::from_u128(13),
            project_id: "project-a",
            mac: "02:00:00:00:00:0b",
            fixed_ip: Ipv4Addr::new(10, 0, 0, 2),
            subnet_cidr: "10.0.0.0/24",
            node_id: "node-a",
            operation_id: Uuid::from_u128(14),
            deadline_unix_ms: 1,
            public_address: None,
            external_realm_id: Some(external_realm_id),
            policies: Vec::new(),
        })?;
        assert!(plan.intents.iter().any(|intent| matches!(
            intent,
            NetworkPlanIntent::Egress(o3k_domain::EgressIntent {
                external_realm_id: id,
                enabled: true,
                nat: true,
            }) if *id == external_realm_id
        )));
        assert!(plan
            .intents
            .iter()
            .any(|intent| matches!(intent, NetworkPlanIntent::EndpointAttachment { endpoint_id: id, .. } if *id == endpoint_id)));
        Ok(())
    }

    #[tokio::test]
    async fn security_group_rules_project_to_endpoint_policy_and_enforce_scope()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("security-groups");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_network(&auth("project-a"), "net".to_owned())
            .await?;
        service
            .create_subnet(
                &auth("project-a"),
                network.id,
                "subnet".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = service
            .create_port(&auth("project-a"), network.id, "port".to_owned())
            .await?;
        let group = service
            .create_security_group_for_project("project-a", "web".to_owned(), String::new())
            .await?;
        let rule = service
            .create_security_group_rule_for_project(
                "project-a",
                group.id,
                "ingress".to_owned(),
                "tcp".to_owned(),
                Some(443),
                Some(443),
                Some("0.0.0.0/0".to_owned()),
            )
            .await?;
        service
            .replace_security_group_bindings_for_project("project-a", port.id, vec![group.id])
            .await?;
        let updated_group = service
            .update_security_group_for_project(
                "project-a",
                group.id,
                "web-renamed".to_owned(),
                "updated".to_owned(),
            )
            .await?;
        assert_eq!(updated_group.name, "web-renamed");
        let canonical_group = store
            .get_reusable_policy("project-a", &group.id)
            .await?
            .ok_or("canonical security group missing")?;
        assert_eq!(canonical_group.generation, 2);
        assert_eq!(canonical_group.stateful_mode, "Stateful");
        assert_eq!(canonical_group.unmatched_action, "Deny");
        let defaults = service
            .policy_defaults_for_endpoint("project-a", port.id)
            .await?;
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].policy_id, group.id);
        assert_eq!(defaults[0].endpoint_id, port.id);
        assert_eq!(defaults[0].unmatched_action, PolicyAction::Deny);
        let default_plan = compile_attachment_plan_with_defaults(
            AttachmentPlanInput {
                endpoint_id: port.id,
                realm_id: network.id,
                project_id: "project-a",
                mac: &port.mac_address,
                fixed_ip: port.fixed_ip,
                subnet_cidr: "192.0.2.0/29",
                node_id: "network-agent-1",
                operation_id: Uuid::now_v7(),
                deadline_unix_ms: 1,
                public_address: None,
                external_realm_id: None,
                policies: Vec::new(),
            },
            defaults,
        )?;
        assert!(default_plan.intents.iter().any(|intent| matches!(
            intent,
            NetworkPlanIntent::PolicyDefault(default)
                if default.policy_id == group.id
                    && default.unmatched_action == PolicyAction::Deny
        )));
        let canonical_rules = store.list_policy_rules("project-a", &group.id).await?;
        assert_eq!(canonical_rules.len(), 1);
        assert_eq!(canonical_rules[0].id, rule.id);
        let canonical_attachments = store
            .list_endpoint_policy_attachments("project-a", &port.id)
            .await?;
        assert_eq!(canonical_attachments.len(), 1);
        assert_eq!(canonical_attachments[0].policy_id, group.id);
        assert_ne!(canonical_attachments[0].id, group.id);
        let policies = service
            .list_policies_for_project("project-a", network.id)
            .await?;
        assert!(policies.iter().any(|policy| policy.id == rule.id
            && policy.endpoint_id == port.id
            && policy.action == PolicyAction::Allow));
        assert!(
            service
                .list_policies_for_project("project-b", network.id)
                .await
                .is_err()
        );
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn gateway_delete_reservation_reconstructs_a_generation_fenced_snapshot()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("gateway-delete-reservation");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let gateway = service
            .create_l3_gateway_for_project("project-a", "edge".to_owned(), None, true)
            .await?;

        let deleting = service
            .delete_l3_gateway_for_project("project-a", &gateway.id, gateway.generation)
            .await?;
        assert_eq!(deleting.state, "deleting");
        assert_eq!(deleting.generation, gateway.generation + 1);

        // A retry/restart can rebuild the exact removal target from the
        // durable reservation; it must not need the pre-delete row in memory.
        let snapshot = service
            .compile_l3_gateway_execution_plan_for_project("project-a", &gateway.id)
            .await?;
        assert_eq!(snapshot.gateway_id, gateway.id);
        assert_eq!(snapshot.gateway_generation, deleting.generation);
        assert!(snapshot.attachments.is_empty());
        assert_eq!(
            store
                .get_canonical_l3_gateway("project-a", &gateway.id)
                .await?
                .ok_or("gateway reservation disappeared")?
                .state,
            "deleting"
        );
        Ok(())
    }

    #[tokio::test]
    async fn attachment_detach_reservation_is_gateway_scoped_and_not_finalized_implicitly()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("gateway-detach-reservation");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_canonical_network_for_project("project-a", "net".to_owned())
            .await?;
        let realm = service
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "192.0.2.0/24".to_owned(),
                false,
            )
            .await?;
        let gateway = service
            .create_l3_gateway_for_project("project-a", "edge".to_owned(), None, true)
            .await?;
        let attachment = service
            .attach_l3_gateway_realm("project-a", &gateway.id, &realm.id)
            .await?;
        let deleting = service
            .detach_l3_gateway_realm("project-a", &attachment.id, attachment.generation)
            .await?;

        assert_eq!(deleting.state, "deleting");
        assert_eq!(deleting.generation, attachment.generation + 1);
        assert!(matches!(
            service
                .attach_l3_gateway_realm("project-a", &gateway.id, &realm.id)
                .await,
            Err(NetworkError::Conflict)
        ));

        // The relation remains present until an external provider observation
        // authorizes finalization, while the gateway snapshot excludes it.
        let snapshot = service
            .compile_l3_gateway_execution_plan_for_project("project-a", &gateway.id)
            .await?;
        assert_eq!(snapshot.gateway_id, gateway.id);
        assert!(snapshot.attachments.is_empty());
        let persisted = store
            .get_canonical_l3_gateway_attachment("project-a", &attachment.id)
            .await?
            .ok_or("attachment reservation disappeared")?;
        assert_eq!(persisted.state, "deleting");
        assert_eq!(persisted.generation, deleting.generation);

        service
            .finalize_l3_gateway_realm_detachment_for_project(
                "project-a",
                &attachment.id,
                deleting.generation,
            )
            .await?;
        assert!(
            store
                .get_canonical_l3_gateway_attachment("project-a", &attachment.id)
                .await?
                .is_none()
        );
        Ok(())
    }
}
