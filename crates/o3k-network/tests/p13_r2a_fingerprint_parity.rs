#![allow(clippy::expect_used)]

use std::{net::Ipv4Addr, sync::Arc};

use o3k_network::CanonicalPolicyService;
use o3k_store::{
    CanonicalAddressRealmRecord, CanonicalEndpointRecord, CanonicalNetworkPolicyRuleRecord,
    CanonicalNetworkRecord, CanonicalPolicyAttachmentRecord, CanonicalReusableNetworkPolicyRecord,
    NetworkRepository, PostgresStore, SqliteStore,
};
use uuid::Uuid;

const PROJECT: &str = "p13-r2a-fingerprint-parity";
const NETWORK: Uuid = Uuid::from_u128(0x100);
const REALM: Uuid = Uuid::from_u128(0x101);
const ENDPOINT: Uuid = Uuid::from_u128(0x102);
const POLICY_A: Uuid = Uuid::from_u128(0x110);
const POLICY_B: Uuid = Uuid::from_u128(0x111);
const ATTACHMENT_A: Uuid = Uuid::from_u128(0x120);
const ATTACHMENT_B: Uuid = Uuid::from_u128(0x121);
const RULE_1: Uuid = Uuid::from_u128(0x130);
const RULE_2: Uuid = Uuid::from_u128(0x131);
const RULE_3: Uuid = Uuid::from_u128(0x132);

struct CompiledGraph {
    snapshot: Vec<o3k_domain::NetworkPlanIntent>,
    fingerprint: String,
    generation: u64,
}

async fn seed<R: NetworkRepository + 'static>(store: Arc<R>, reverse_order: bool) -> CompiledGraph {
    store
        .insert_canonical_network(&CanonicalNetworkRecord {
            id: NETWORK,
            project_id: PROJECT.into(),
            name: "parity".into(),
            admin_state_up: true,
            generation: 3,
            state: "active".into(),
        })
        .await
        .expect("network");
    store
        .insert_canonical_realm(&CanonicalAddressRealmRecord {
            id: REALM,
            network_id: NETWORK,
            project_id: PROJECT.into(),
            prefix: "10.42.0.0/24".into(),
            overlapping_prefixes: false,
            generation: 4,
            state: "active".into(),
        })
        .await
        .expect("realm");
    store
        .insert_canonical_endpoint(&CanonicalEndpointRecord {
            id: ENDPOINT,
            realm_id: REALM,
            project_id: PROJECT.into(),
            fixed_ip: Ipv4Addr::new(10, 42, 0, 10),
            mac: "02:00:00:42:00:10".into(),
            generation: 5,
            state: "active".into(),
        })
        .await
        .expect("endpoint");

    let policy = |id, generation| CanonicalReusableNetworkPolicyRecord {
        id,
        project_id: PROJECT.into(),
        name: format!("policy-{id}"),
        description: String::new(),
        stateful_mode: "Stateful".into(),
        unmatched_action: "Deny".into(),
        generation,
        state: "active".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    };
    let policies = [policy(POLICY_A, 6), policy(POLICY_B, 7)];
    for index in [0usize, 1usize] {
        let index = if reverse_order { 1 - index } else { index };
        store
            .insert_reusable_policy(&policies[index])
            .await
            .expect("policy");
    }

    let rule = |id, policy_id, port, generation| CanonicalNetworkPolicyRuleRecord {
        id,
        policy_id,
        project_id: PROJECT.into(),
        direction: "Ingress".into(),
        address_family: "Ipv4".into(),
        protocol: "Tcp".into(),
        port_min: Some(port),
        port_max: Some(port),
        remote_selector: Some("198.51.100.0/24".into()),
        action: "Allow".into(),
        state: "active".into(),
        generation,
        enforcement_key: format!("Ingress|Ipv4|Tcp|{port}-{port}|198.51.100.0/24|Allow"),
    };
    let rules = [
        rule(RULE_1, POLICY_A, 8001, 8),
        rule(RULE_2, POLICY_B, 8002, 9),
        rule(RULE_3, POLICY_A, 8443, 10),
    ];
    let order = if reverse_order {
        [2usize, 0usize, 1usize]
    } else {
        [0usize, 1usize, 2usize]
    };
    for index in order {
        store.insert_policy_rule(&rules[index]).await.expect("rule");
    }
    let attachments = [
        CanonicalPolicyAttachmentRecord {
            id: ATTACHMENT_A,
            policy_id: POLICY_A,
            endpoint_id: ENDPOINT,
            project_id: PROJECT.into(),
            state: "active".into(),
            generation: 11,
        },
        CanonicalPolicyAttachmentRecord {
            id: ATTACHMENT_B,
            policy_id: POLICY_B,
            endpoint_id: ENDPOINT,
            project_id: PROJECT.into(),
            state: "active".into(),
            generation: 12,
        },
    ];
    for index in if reverse_order {
        [1usize, 0usize]
    } else {
        [0usize, 1usize]
    } {
        store
            .insert_policy_attachment(&attachments[index])
            .await
            .expect("attachment");
    }

    let service = CanonicalPolicyService::new(store);
    let (snapshot, fingerprint, generation) = service
        .compile_endpoint_with_metadata(PROJECT, ENDPOINT)
        .await
        .expect("compile");
    CompiledGraph {
        snapshot,
        fingerprint,
        generation,
    }
}

#[tokio::test]
async fn sqlite_insertion_order_does_not_change_fingerprint() {
    let first = seed(
        Arc::new(
            SqliteStore::connect("sqlite::memory:")
                .await
                .expect("sqlite"),
        ),
        false,
    )
    .await;
    let second = seed(
        Arc::new(
            SqliteStore::connect("sqlite::memory:")
                .await
                .expect("sqlite"),
        ),
        true,
    )
    .await;
    assert_eq!(first.snapshot, second.snapshot);
    assert_eq!(first.fingerprint, second.fingerprint);
    assert_eq!(first.generation, second.generation);
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL conformance database"]
async fn postgres_insertion_order_does_not_change_fingerprint() {
    let url = std::env::var("O3K_DATABASE_URL").expect("O3K_DATABASE_URL");
    let first_store = Arc::new(PostgresStore::connect(&url).await.expect("postgres"));
    let first = seed(first_store, false).await;
    // The conformance database is disposable; this second graph uses the same
    // values so the comparison is against the same canonical identity.
    let second_url = std::env::var("O3K_DATABASE_URL_PARITY").expect("O3K_DATABASE_URL_PARITY");
    let second_store = Arc::new(PostgresStore::connect(&second_url).await.expect("postgres"));
    let second = seed(second_store, true).await;
    assert_eq!(first.snapshot, second.snapshot);
    assert_eq!(first.fingerprint, second.fingerprint);
    assert_eq!(first.generation, second.generation);
}
