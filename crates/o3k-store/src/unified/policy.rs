use super::*;

#[async_trait]
impl CanonicalPolicyRepository for O3kStore {
    async fn insert_reusable_policy(
        &self,
        p: &CanonicalReusableNetworkPolicyRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_reusable_policy(p).await,
            Self::Postgres(s) => s.insert_reusable_policy(p).await,
        }
    }
    async fn get_reusable_policy(
        &self,
        project: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalReusableNetworkPolicyRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_reusable_policy(project, id).await,
            Self::Postgres(s) => s.get_reusable_policy(project, id).await,
        }
    }
    async fn list_reusable_policies(
        &self,
        project: &str,
    ) -> Result<Vec<CanonicalReusableNetworkPolicyRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_reusable_policies(project).await,
            Self::Postgres(s) => s.list_reusable_policies(project).await,
        }
    }
    async fn update_reusable_policy(
        &self,
        p: &CanonicalReusableNetworkPolicyRecord,
        generation: u64,
    ) -> Result<CanonicalReusableNetworkPolicyRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.update_reusable_policy(p, generation).await,
            Self::Postgres(s) => s.update_reusable_policy(p, generation).await,
        }
    }
    async fn transition_reusable_policy_state(
        &self,
        project: &str,
        id: &Uuid,
        generation: u64,
        state: &str,
    ) -> Result<CanonicalReusableNetworkPolicyRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.transition_reusable_policy_state(project, id, generation, state)
                    .await
            }
            Self::Postgres(s) => {
                s.transition_reusable_policy_state(project, id, generation, state)
                    .await
            }
        }
    }
    async fn delete_reusable_policy(&self, project: &str, id: &Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_reusable_policy(project, id).await,
            Self::Postgres(s) => s.delete_reusable_policy(project, id).await,
        }
    }
    async fn insert_policy_rule(
        &self,
        r: &CanonicalNetworkPolicyRuleRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_policy_rule(r).await,
            Self::Postgres(s) => s.insert_policy_rule(r).await,
        }
    }
    async fn get_policy_rule(
        &self,
        project: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalNetworkPolicyRuleRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_policy_rule(project, id).await,
            Self::Postgres(s) => s.get_policy_rule(project, id).await,
        }
    }
    async fn list_policy_rules(
        &self,
        project: &str,
        policy: &Uuid,
    ) -> Result<Vec<CanonicalNetworkPolicyRuleRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_policy_rules(project, policy).await,
            Self::Postgres(s) => s.list_policy_rules(project, policy).await,
        }
    }
    async fn list_deleting_policy_rules(
        &self,
    ) -> Result<Vec<CanonicalNetworkPolicyRuleRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_deleting_policy_rules().await,
            Self::Postgres(s) => s.list_deleting_policy_rules().await,
        }
    }
    async fn begin_policy_rule_deletion(
        &self,
        project: &str,
        id: &Uuid,
        generation: u64,
    ) -> Result<CanonicalNetworkPolicyRuleRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.begin_policy_rule_deletion(project, id, generation).await,
            Self::Postgres(s) => s.begin_policy_rule_deletion(project, id, generation).await,
        }
    }
    async fn finalize_policy_rule_deletion(
        &self,
        project: &str,
        id: &Uuid,
        generation: u64,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.finalize_policy_rule_deletion(project, id, generation)
                    .await
            }
            Self::Postgres(s) => {
                s.finalize_policy_rule_deletion(project, id, generation)
                    .await
            }
        }
    }
    async fn delete_policy_rule(&self, project: &str, id: &Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_policy_rule(project, id).await,
            Self::Postgres(s) => s.delete_policy_rule(project, id).await,
        }
    }
    async fn insert_policy_attachment(
        &self,
        a: &CanonicalPolicyAttachmentRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_policy_attachment(a).await,
            Self::Postgres(s) => s.insert_policy_attachment(a).await,
        }
    }
    async fn get_policy_attachment(
        &self,
        project: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalPolicyAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_policy_attachment(project, id).await,
            Self::Postgres(s) => s.get_policy_attachment(project, id).await,
        }
    }
    async fn list_policy_attachments(
        &self,
        project: &str,
        policy: &Uuid,
    ) -> Result<Vec<CanonicalPolicyAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_policy_attachments(project, policy).await,
            Self::Postgres(s) => s.list_policy_attachments(project, policy).await,
        }
    }
    async fn list_endpoint_policy_attachments(
        &self,
        project: &str,
        endpoint: &Uuid,
    ) -> Result<Vec<CanonicalPolicyAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_endpoint_policy_attachments(project, endpoint).await,
            Self::Postgres(s) => s.list_endpoint_policy_attachments(project, endpoint).await,
        }
    }
    async fn list_deleting_policy_attachments(
        &self,
    ) -> Result<Vec<CanonicalPolicyAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_deleting_policy_attachments().await,
            Self::Postgres(s) => s.list_deleting_policy_attachments().await,
        }
    }
    async fn replace_policy_attachment_set(
        &self,
        project: &str,
        endpoint: &Uuid,
        policy_ids: &[Uuid],
    ) -> Result<Vec<CanonicalPolicyAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.replace_policy_attachment_set(project, endpoint, policy_ids)
                    .await
            }
            Self::Postgres(s) => {
                s.replace_policy_attachment_set(project, endpoint, policy_ids)
                    .await
            }
        }
    }
    async fn begin_policy_attachment_deletion(
        &self,
        project: &str,
        id: &Uuid,
        generation: u64,
    ) -> Result<CanonicalPolicyAttachmentRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.begin_policy_attachment_deletion(project, id, generation)
                    .await
            }
            Self::Postgres(s) => {
                s.begin_policy_attachment_deletion(project, id, generation)
                    .await
            }
        }
    }
    async fn finalize_policy_attachment_deletion(
        &self,
        project: &str,
        id: &Uuid,
        generation: u64,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.finalize_policy_attachment_deletion(project, id, generation)
                    .await
            }
            Self::Postgres(s) => {
                s.finalize_policy_attachment_deletion(project, id, generation)
                    .await
            }
        }
    }
    async fn delete_policy_attachment(&self, project: &str, id: &Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_policy_attachment(project, id).await,
            Self::Postgres(s) => s.delete_policy_attachment(project, id).await,
        }
    }
    async fn upsert_policy_realization(
        &self,
        realization: &CanonicalPolicyRealizationRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.upsert_policy_realization(realization).await,
            Self::Postgres(s) => s.upsert_policy_realization(realization).await,
        }
    }
    async fn get_policy_realization(
        &self,
        project: &str,
        endpoint: &Uuid,
    ) -> Result<Option<CanonicalPolicyRealizationRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_policy_realization(project, endpoint).await,
            Self::Postgres(s) => s.get_policy_realization(project, endpoint).await,
        }
    }
    async fn list_policy_realizations(
        &self,
        project: &str,
    ) -> Result<Vec<CanonicalPolicyRealizationRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_policy_realizations(project).await,
            Self::Postgres(s) => s.list_policy_realizations(project).await,
        }
    }
    async fn set_policy_realization_outcome(
        &self,
        project: &str,
        endpoint: &Uuid,
        expected: &str,
        attempt_id: &Uuid,
        state: &str,
        observed: Option<&str>,
        generation: Option<u64>,
        provider_resource_id: Option<&str>,
        last_outcome: Option<&str>,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.set_policy_realization_outcome(
                    project,
                    endpoint,
                    expected,
                    attempt_id,
                    state,
                    observed,
                    generation,
                    provider_resource_id,
                    last_outcome,
                )
                .await
            }
            Self::Postgres(s) => {
                s.set_policy_realization_outcome(
                    project,
                    endpoint,
                    expected,
                    attempt_id,
                    state,
                    observed,
                    generation,
                    provider_resource_id,
                    last_outcome,
                )
                .await
            }
        }
    }

    async fn requeue_policy_realization(
        &self,
        expected_attempt_id: &Uuid,
        realization: &CanonicalPolicyRealizationRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.requeue_policy_realization(expected_attempt_id, realization)
                    .await
            }
            Self::Postgres(s) => {
                s.requeue_policy_realization(expected_attempt_id, realization)
                    .await
            }
        }
    }
}
