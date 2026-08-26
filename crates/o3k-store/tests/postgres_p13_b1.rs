use o3k_store::{
    CanonicalAddressRealmRecord, CanonicalEndpointRecord, CanonicalNetworkPolicyRuleRecord,
    CanonicalNetworkRecord, CanonicalPolicyAttachmentRecord, CanonicalPolicyRealizationRecord,
    CanonicalReusableNetworkPolicyRecord, PostgresStore, StoreError,
};
use std::net::Ipv4Addr;
use uuid::Uuid;

fn database_url() -> Result<String, StoreError> {
    std::env::var("O3K_DATABASE_URL")
        .map_err(|_| StoreError::Corrupt("O3K_DATABASE_URL is required".into()))
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
    let url = database_url()?;
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
            .ok_or(StoreError::Corrupt("policy missing after reopen".into()))?
            .id,
        policy_id
    );
    assert_eq!(
        reopened
            .get_policy_rule("p13-b1-project", &rule_id)
            .await?
            .ok_or(StoreError::Corrupt("rule missing after reopen".into()))?
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

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL instance"]
async fn postgres_p13_b1_attachment_lifecycle_and_races() -> Result<(), StoreError> {
    let url = database_url()?;
    let store = PostgresStore::connect(&url).await?;
    let project = "p13-b1-attachment-project";
    let network_id = Uuid::now_v7();
    let realm_id = Uuid::now_v7();
    let endpoint_id = Uuid::now_v7();
    let policy_id = Uuid::now_v7();
    let policy_two_id = Uuid::now_v7();
    let race_policy_id = Uuid::now_v7();
    let conflict_policy_id = Uuid::now_v7();
    let rule_id = Uuid::now_v7();
    let attachment_id = Uuid::now_v7();

    store
        .insert_canonical_network(&CanonicalNetworkRecord {
            id: network_id,
            project_id: project.into(),
            name: "network".into(),
            admin_state_up: true,
            generation: 1,
            state: "active".into(),
        })
        .await?;
    store
        .insert_canonical_realm(&CanonicalAddressRealmRecord {
            id: realm_id,
            network_id,
            project_id: project.into(),
            prefix: "10.20.0.0/24".into(),
            overlapping_prefixes: false,
            generation: 1,
            state: "active".into(),
        })
        .await?;
    store
        .insert_canonical_endpoint(&CanonicalEndpointRecord {
            id: endpoint_id,
            realm_id,
            project_id: project.into(),
            fixed_ip: Ipv4Addr::new(
                10,
                20,
                endpoint_id.as_bytes()[12] % 250 + 1,
                endpoint_id.as_bytes()[13] % 250 + 1,
            ),
            mac: format!(
                "02:00:{:02x}:{:02x}:{:02x}:{:02x}",
                endpoint_id.as_bytes()[12],
                endpoint_id.as_bytes()[13],
                endpoint_id.as_bytes()[14],
                endpoint_id.as_bytes()[15]
            ),
            generation: 1,
            state: "active".into(),
        })
        .await?;
    let mut p = policy(policy_id);
    p.project_id = project.into();
    store.insert_reusable_policy(&p).await?;
    let mut p2 = policy(policy_two_id);
    p2.project_id = project.into();
    store.insert_reusable_policy(&p2).await?;
    let mut race_policy = policy(race_policy_id);
    race_policy.project_id = project.into();
    store.insert_reusable_policy(&race_policy).await?;
    let mut conflict_policy = policy(conflict_policy_id);
    conflict_policy.project_id = project.into();
    conflict_policy.unmatched_action = "Allow".into();
    store.insert_reusable_policy(&conflict_policy).await?;

    let rule = CanonicalNetworkPolicyRuleRecord {
        id: rule_id,
        policy_id,
        project_id: project.into(),
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
    let second_store = PostgresStore::connect(&url).await?;
    let race_rule = CanonicalNetworkPolicyRuleRecord {
        id: Uuid::now_v7(),
        policy_id: race_policy_id,
        ..rule.clone()
    };
    let race_rule_two = CanonicalNetworkPolicyRuleRecord {
        id: Uuid::now_v7(),
        ..race_rule.clone()
    };
    let (first, second) = tokio::join!(
        store.insert_policy_rule(&race_rule),
        second_store.insert_policy_rule(&race_rule_two)
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert!(
        matches!(first, Err(StoreError::ResourceAlreadyExists))
            || matches!(second, Err(StoreError::ResourceAlreadyExists))
    );

    let attachment = CanonicalPolicyAttachmentRecord {
        id: attachment_id,
        policy_id,
        endpoint_id,
        project_id: project.into(),
        state: "active".into(),
        generation: 1,
    };
    store.insert_policy_attachment(&attachment).await?;
    let compatible_attachment = CanonicalPolicyAttachmentRecord {
        id: Uuid::now_v7(),
        policy_id: policy_two_id,
        ..attachment.clone()
    };
    store
        .insert_policy_attachment(&compatible_attachment)
        .await?;
    assert_eq!(
        store
            .list_endpoint_policy_attachments(project, &endpoint_id)
            .await?
            .len(),
        2
    );
    store
        .upsert_policy_realization(&CanonicalPolicyRealizationRecord {
            endpoint_id,
            project_id: project.into(),
            desired_fingerprint: "sha256:p13-b2".into(),
            desired_generation: 3,
            observed_fingerprint: None,
            observed_generation: None,
            state: "unknown".into(),
            provider_resource_id: None,
            last_outcome: Some("transport loss".into()),
        })
        .await?;
    assert!(matches!(
        store
            .insert_policy_attachment(&CanonicalPolicyAttachmentRecord {
                id: Uuid::now_v7(),
                policy_id: conflict_policy_id,
                ..attachment.clone()
            })
            .await,
        Err(StoreError::PolicyCompositionConflict)
    ));
    drop(store);
    let reopened = PostgresStore::connect(&url).await?;
    let loaded = reopened
        .get_policy_attachment(project, &attachment_id)
        .await?
        .ok_or(StoreError::Corrupt(
            "attachment missing after reopen".into(),
        ))?;
    assert_eq!(
        (
            loaded.id,
            loaded.policy_id,
            loaded.endpoint_id,
            loaded.generation
        ),
        (attachment_id, policy_id, endpoint_id, 1)
    );
    assert_eq!(
        reopened
            .list_policy_attachments(project, &policy_id)
            .await?
            .len(),
        1
    );
    assert_eq!(
        reopened
            .list_endpoint_policy_attachments(project, &endpoint_id)
            .await?
            .len(),
        2
    );
    let realization = reopened
        .get_policy_realization(project, &endpoint_id)
        .await?
        .ok_or(StoreError::Corrupt(
            "realization missing after reopen".into(),
        ))?;
    assert_eq!(
        (realization.desired_generation, realization.state.as_str()),
        (3, "unknown")
    );

    let deleting_compatible = reopened
        .begin_policy_attachment_deletion(project, &compatible_attachment.id, 1)
        .await?;
    reopened
        .finalize_policy_attachment_deletion(
            project,
            &compatible_attachment.id,
            deleting_compatible.generation,
        )
        .await?;
    let deleting_initial = reopened
        .begin_policy_attachment_deletion(project, &attachment_id, 1)
        .await?;
    reopened
        .finalize_policy_attachment_deletion(project, &attachment_id, deleting_initial.generation)
        .await?;

    let duplicate_attachment = CanonicalPolicyAttachmentRecord {
        id: Uuid::now_v7(),
        ..attachment.clone()
    };
    let concurrent_attachment = CanonicalPolicyAttachmentRecord {
        id: Uuid::now_v7(),
        ..attachment.clone()
    };
    let (first, second) = tokio::join!(
        reopened.insert_policy_attachment(&duplicate_attachment),
        second_store.insert_policy_attachment(&concurrent_attachment)
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert!(
        matches!(first, Err(StoreError::ResourceAlreadyExists))
            || matches!(second, Err(StoreError::ResourceAlreadyExists))
    );
    let winner = if first.is_ok() {
        duplicate_attachment.id
    } else {
        concurrent_attachment.id
    };
    let deleting = reopened
        .begin_policy_attachment_deletion(project, &winner, 1)
        .await?;
    assert_eq!(
        (deleting.state.as_str(), deleting.generation),
        ("deleting", 2)
    );
    assert!(matches!(
        reopened
            .finalize_policy_attachment_deletion(project, &winner, 1)
            .await,
        Err(StoreError::StaleGeneration)
    ));
    reopened
        .finalize_policy_attachment_deletion(project, &winner, 2)
        .await?;
    assert!(
        reopened
            .get_policy_attachment(project, &winner)
            .await?
            .is_none()
    );
    assert!(matches!(
        reopened
            .transition_reusable_policy_state(project, &policy_id, 1, "deleting")
            .await,
        Err(StoreError::NetworkInUse)
    ));
    let deleting_rule = reopened
        .begin_policy_rule_deletion(project, &rule_id, 1)
        .await?;
    reopened
        .finalize_policy_rule_deletion(project, &rule_id, deleting_rule.generation)
        .await?;
    let remaining_rule = reopened
        .list_policy_rules(project, &race_policy_id)
        .await?
        .remove(0);
    let deleting_race_rule = reopened
        .begin_policy_rule_deletion(project, &remaining_rule.id, remaining_rule.generation)
        .await?;
    reopened
        .finalize_policy_rule_deletion(project, &remaining_rule.id, deleting_race_rule.generation)
        .await?;
    let deleting_policy = reopened
        .transition_reusable_policy_state(project, &policy_id, 1, "deleting")
        .await?;
    assert_eq!(deleting_policy.generation, 2);
    assert!(matches!(
        reopened
            .insert_policy_rule(&CanonicalNetworkPolicyRuleRecord {
                id: Uuid::now_v7(),
                policy_id,
                project_id: project.into(),
                state: "active".into(),
                generation: 1,
                ..rule.clone()
            })
            .await,
        Err(StoreError::OwnershipConflict)
    ));
    reopened.delete_reusable_policy(project, &policy_id).await?;
    reopened
        .delete_reusable_policy(project, &policy_two_id)
        .await?;
    reopened
        .delete_reusable_policy(project, &race_policy_id)
        .await?;
    reopened
        .delete_reusable_policy(project, &conflict_policy_id)
        .await?;
    Ok(())
}
