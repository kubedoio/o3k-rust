#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::{path::PathBuf, sync::Arc};

use o3k_domain::{NetworkProtocol, PolicyAction, PolicyDirection, PolicyIntent, PortRange};
use o3k_network::NetworkService;
use uuid::Uuid;

fn root() -> PathBuf {
    std::env::temp_dir().join(format!("o3k-p13-f3-{}", Uuid::new_v4()))
}

#[tokio::test]
async fn fresh_runtime_reconstructs_canonical_hierarchy_without_network_intent()
-> Result<(), Box<dyn std::error::Error>> {
    let root = root();
    let db = root.with_extension("sqlite");
    let service_root = root.with_extension("runtime");
    let project = "p13-f3-runtime";

    let first_store = Arc::new(o3k_store::testkit::open_file(&db).await?);
    let first = NetworkService::open(&service_root, first_store.clone()).await?;
    let network = first
        .create_canonical_network_for_project(project, "restart-proof".into())
        .await?;
    let empty = first
        .reconstruct_canonical_network(project, network.id)
        .await?;
    assert_eq!(empty.network.id, network.id);
    assert!(empty.realms.is_empty());

    let realm_a = first
        .create_canonical_realm_for_project(project, network.id, "10.20.0.0/24".into(), true)
        .await?;
    let realm_b = first
        .create_canonical_realm_for_project(project, network.id, "10.20.0.0/24".into(), true)
        .await?;
    let pool = first
        .create_canonical_pool_for_project(
            project,
            realm_a.id,
            "10.20.0.0/24".into(),
            Some("10.20.0.1".parse()?),
            "10.20.0.2".parse()?,
            "10.20.0.254".parse()?,
        )
        .await?;
    let endpoint_a = first
        .create_canonical_endpoint_for_project(
            project,
            realm_a.id,
            "10.20.0.10".parse()?,
            "02:00:00:20:00:0a".into(),
        )
        .await?;
    let endpoint_b = first
        .create_canonical_endpoint_for_project(
            project,
            realm_b.id,
            "10.20.0.10".parse()?,
            "02:00:00:20:00:0b".into(),
        )
        .await?;
    let policy = PolicyIntent {
        id: Uuid::now_v7(),
        endpoint_id: endpoint_a.id,
        direction: PolicyDirection::Ingress,
        protocol: NetworkProtocol::Tcp,
        ports: Some(PortRange {
            start: 443,
            end: 443,
        }),
        source: None,
        destination: None,
        action: PolicyAction::Allow,
    };
    first
        .upsert_policy_for_project(project, network.id, policy.clone())
        .await?;
    assert!(
        first_store
            .get_network_intent(project, &network.id)
            .await?
            .is_none()
    );
    drop(first);
    drop(first_store);

    let second_store = Arc::new(o3k_store::testkit::open_file(&db).await?);
    let second = NetworkService::open(&service_root, second_store.clone()).await?;
    let snapshot = second
        .reconstruct_canonical_network(project, network.id)
        .await?;
    assert_eq!(snapshot.network, network);
    assert_eq!(snapshot.realms.len(), 2);
    assert_eq!(snapshot.pools[&realm_a.id], vec![pool.clone()]);
    assert_eq!(snapshot.endpoints[&realm_a.id], vec![endpoint_a.clone()]);
    assert_eq!(snapshot.endpoints[&realm_b.id], vec![endpoint_b.clone()]);
    assert_eq!(endpoint_a.fixed_ip, endpoint_b.fixed_ip);
    assert_eq!(
        second
            .list_policies_for_project(project, network.id)
            .await?,
        vec![policy]
    );

    second_store
        .delete_canonical_endpoint(project, &endpoint_a.id)
        .await?;
    second_store
        .delete_canonical_endpoint(project, &endpoint_b.id)
        .await?;
    second_store
        .delete_canonical_pool(project, &pool.id)
        .await?;
    second
        .delete_canonical_realm_for_project(project, realm_a.id)
        .await?;
    second
        .delete_canonical_realm_for_project(project, realm_b.id)
        .await?;
    drop(second);
    drop(second_store);

    let final_store = Arc::new(o3k_store::testkit::open_file(&db).await?);
    let final_service = NetworkService::open(&service_root, final_store).await?;
    let final_snapshot = final_service
        .reconstruct_canonical_network(project, network.id)
        .await?;
    assert_eq!(final_snapshot.network.id, network.id);
    assert!(final_snapshot.realms.is_empty());
    assert!(
        final_service
            .list_policies_for_project(project, network.id)
            .await?
            .is_empty()
    );

    let _ = std::fs::remove_dir_all(&service_root);
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(format!("{}-wal", db.display()));
    let _ = std::fs::remove_file(format!("{}-shm", db.display()));
    Ok(())
}
