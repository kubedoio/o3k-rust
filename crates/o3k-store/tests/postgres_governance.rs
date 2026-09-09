#![allow(clippy::expect_used, clippy::unwrap_used)]

use o3k_store::IdentityRepository;
use o3k_store::{
    AuditEventRecord, CanonicalOperationRecord, GovernanceRepository, IdempotencyReservation,
    IdempotencyReservationRequest, KeystoneDomainRecord, KeystoneProjectRecord,
    KeystoneRoleAssignmentRecord, KeystoneRoleRecord, KeystoneUserRecord, OperationRecord,
    OperationState, PostgresStore,
};
use sqlx::Row;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires the configured PostgreSQL conformance database"]
async fn postgres_role_assignment_mutation_is_atomic_replay_safe_and_scope_isolated() {
    let url = match std::env::var("O3K_DATABASE_URL") {
        Ok(url) => url,
        Err(_) => return,
    };
    let store = PostgresStore::connect(&url).await.expect("connect");
    store.clean_tables_for_testing().await.expect("clean");
    let now = "2026-01-01T00:00:00Z";
    store
        .insert_keystone_domain(&KeystoneDomainRecord {
            id: "gd".into(),
            name: "Governance Test".into(),
            description: None,
            enabled: true,
            created_at: now.into(),
        })
        .await
        .expect("domain");
    for (id, name) in [("pa", "Project A"), ("pb", "Project B")] {
        store
            .insert_keystone_project(&KeystoneProjectRecord {
                id: id.into(),
                domain_id: "gd".into(),
                name: name.into(),
                description: None,
                enabled: true,
                created_at: now.into(),
            })
            .await
            .expect("project");
    }
    for (id, name) in [("ua", "User A"), ("ub", "User B")] {
        store
            .insert_keystone_user(&KeystoneUserRecord {
                id: id.into(),
                domain_id: "gd".into(),
                name: name.into(),
                password_hash: "test-only".into(),
                email: None,
                enabled: true,
                created_at: now.into(),
            })
            .await
            .expect("user");
    }
    store
        .insert_keystone_role(&KeystoneRoleRecord {
            id: "rr".into(),
            name: "reader".into(),
            description: None,
            created_at: now.into(),
        })
        .await
        .expect("role");

    let store_ref = &store;
    let mutate = |assignment: KeystoneRoleAssignmentRecord,
                  operation_id: Uuid,
                  key: String,
                  owner: String| {
        let store = store_ref;
        async move {
            let assignment_id = Uuid::parse_str(&assignment.id).expect("assignment UUID");
            let operation = OperationRecord {
                id: operation_id,
                resource_id: assignment_id,
                kind: "iam:assign_role".into(),
                state: OperationState::Succeeded,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            };
            let canonical = CanonicalOperationRecord {
                id: operation_id,
                service: "iam".into(),
                action: "iam:AssignRole".into(),
                actor: "operator".into(),
                owner_scope: owner.clone(),
                resource_type: "iam:role_assignment".into(),
                resource_id: Some(assignment.id.clone()),
                state: OperationState::Succeeded,
                attempt: 1,
                created_at: now.into(),
                started_at: None,
                finished_at: Some(now.into()),
                error: None,
                request_id: Some(format!("req-{owner}")),
            };
            let request = IdempotencyReservationRequest::from_semantics(&owner, "iam:AssignRole", &key, "iam:role_assignment", Some(&assignment.id), &serde_json::json!({"principal": assignment.user_id, "project": assignment.project_id, "role": assignment.role_id}), operation_id).expect("request");
            let audit = AuditEventRecord {
                event_id: format!("audit-{owner}-{key}"),
                timestamp: now.into(),
                request_id: format!("req-{owner}"),
                audit_id: format!("audit-{owner}"),
                principal_id: "operator".into(),
                effective_scope: "system".into(),
                service_namespace: "iam".into(),
                action: "iam:AssignRole".into(),
                resource_type: Some("iam:role_assignment".into()),
                resource_id: Some(assignment.id.clone()),
                owner_scope: Some(owner),
                operation_id: Some(operation_id),
                outcome: "succeeded".into(),
                reason_category: None,
                event_json: "{}".into(),
            };
            store
                .mutate_role_assignment(&assignment, &operation, &canonical, &request, &audit)
                .await
        }
    };
    let a = KeystoneRoleAssignmentRecord {
        id: Uuid::new_v4().to_string(),
        user_id: "ua".into(),
        project_id: "pa".into(),
        role_id: "rr".into(),
        created_at: now.into(),
    };
    let op_a = Uuid::new_v4();
    assert_eq!(
        mutate(a.clone(), op_a, "same-key".into(), "pa".into())
            .await
            .expect("first"),
        IdempotencyReservation::Created(op_a)
    );
    assert_eq!(
        mutate(a.clone(), op_a, "same-key".into(), "pa".into())
            .await
            .expect("replay"),
        IdempotencyReservation::ExistingEquivalent(op_a)
    );
    let conflict = KeystoneRoleAssignmentRecord {
        id: Uuid::new_v4().to_string(),
        ..a.clone()
    };
    assert_eq!(
        mutate(conflict, Uuid::new_v4(), "same-key".into(), "pa".into())
            .await
            .expect("conflict"),
        IdempotencyReservation::Conflict
    );
    let b = KeystoneRoleAssignmentRecord {
        id: Uuid::new_v4().to_string(),
        user_id: "ub".into(),
        project_id: "pb".into(),
        role_id: "rr".into(),
        created_at: now.into(),
    };
    let op_b = Uuid::new_v4();
    assert_eq!(
        mutate(b, op_b, "same-key".into(), "pb".into())
            .await
            .expect("project B"),
        IdempotencyReservation::Created(op_b)
    );
    let counts = sqlx::query("SELECT (SELECT COUNT(*) FROM keystone_role_assignments) AS assignments, (SELECT COUNT(*) FROM operations) AS operations, (SELECT COUNT(*) FROM audit_events) AS audits")
        .fetch_one(store.pool()).await.expect("counts");
    assert_eq!(counts.get::<i64, _>("assignments"), 2);
    assert_eq!(counts.get::<i64, _>("operations"), 2);
    assert_eq!(counts.get::<i64, _>("audits"), 2);

    // Recovery evidence: close the pool and reconstruct a fresh store.  The
    // assignment, canonical operation, and audit rows must remain jointly
    // visible after a controller/database-client restart.
    drop(store);
    let reopened = PostgresStore::connect(&url).await.expect("reconnect");
    let recovered = sqlx::query(
        "SELECT (SELECT COUNT(*) FROM keystone_role_assignments) AS assignments,\
                (SELECT COUNT(*) FROM canonical_operation_metadata) AS operations,\
                (SELECT COUNT(*) FROM audit_events) AS audits",
    )
    .fetch_one(reopened.pool())
    .await
    .expect("recovery counts");
    assert_eq!(recovered.get::<i64, _>("assignments"), 2);
    assert_eq!(recovered.get::<i64, _>("operations"), 2);
    assert_eq!(recovered.get::<i64, _>("audits"), 2);
}
