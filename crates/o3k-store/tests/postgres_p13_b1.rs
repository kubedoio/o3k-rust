use o3k_store::{
    CanonicalNetworkPolicyRuleRecord, CanonicalReusableNetworkPolicyRecord, PostgresStore,
    StoreError,
};
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("O3K_DATABASE_URL")
        .expect("O3K_DATABASE_URL must be set for PostgreSQL P13.3B1 conformance")
}

fn policy(id: Uuid) -> CanonicalReusableNetworkPolicyRecord {
    CanonicalReusableNetworkPolicyRecord {
        id,
        project_id: "p13-b1-project".into(),
        name: "detached-policy".into(),
        description: "P13.3B1".into(),
        stateful_mode: "Stateful".into(),
        unmatched_action: "Deny".into(),
        generation: 1,
        state: "active".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    }
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL instance"]
async fn postgres_p13_b1_policy_identity_and_rule_persist_across_reopen() -> Result<(), StoreError>
{
    let url = database_url();
    let store = PostgresStore::connect(&url).await?;
    let policy_id = Uuid::now_v7();
    let rule_id = Uuid::now_v7();
    store.insert_reusable_policy(&policy(policy_id)).await?;
    let rule = CanonicalNetworkPolicyRuleRecord {
        id: rule_id,
        policy_id,
        project_id: "p13-b1-project".into(),
        direction: "Ingress".into(),
        address_family: "Ipv4".into(),
        protocol: "Tcp".into(),
        port_min: Some(443),
        port_max: Some(443),
        remote_selector: Some("198.51.100.0/24".into()),
        action: "Allow".into(),
        state: "active".into(),
        generation: 1,
        enforcement_key: "Ingress|Ipv4|Tcp|443-443|198.51.100.0/24|Allow".into(),
    };
    store.insert_policy_rule(&rule).await?;
    assert_eq!(
        store
            .list_policy_rules("p13-b1-project", &policy_id)
            .await?
            .len(),
        1
    );
    drop(store);
    let reopened = PostgresStore::connect(&url).await?;
    assert_eq!(
        reopened
            .get_reusable_policy("p13-b1-project", &policy_id)
            .await?
            .unwrap()
            .id,
        policy_id
    );
    assert_eq!(
        reopened
            .get_policy_rule("p13-b1-project", &rule_id)
            .await?
            .unwrap()
            .id,
        rule_id
    );
    reopened
        .delete_policy_rule("p13-b1-project", &rule_id)
        .await?;
    reopened
        .delete_reusable_policy("p13-b1-project", &policy_id)
        .await?;
    Ok(())
}
