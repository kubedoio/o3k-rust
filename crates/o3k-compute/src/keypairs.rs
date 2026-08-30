use super::{
    AuthContext, ComputeError, ComputeService, Keypair, ResourceId, ResourceTarget, ResourceType,
    StoreError, Uuid, keypair_from_record, validate_keypair_name,
};

use o3k_kernel::{ActionId, AuditEvent, AuditOutcome, AuthorizationRequest, ServiceNamespace};
use std::time::{SystemTime, UNIX_EPOCH};

impl ComputeService {
    pub async fn create_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: String,
        public_key: String,
    ) -> Result<Keypair, ComputeError> {
        validate_keypair_name(&name)?;
        let (key_type, fingerprint, public_key) =
            o3k_store::validate_public_key(&public_key).map_err(ComputeError::Store)?;
        let record = o3k_store::KeypairRecord {
            id: Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("o3k:keypair:{user_id}:{project_id}:{name}").as_bytes(),
            ),
            user_id: user_id.to_owned(),
            project_id: project_id.to_owned(),
            name,
            key_type,
            public_key,
            fingerprint,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| ComputeError::InvalidRequest)?
                .as_secs()
                .to_string(),
        };
        self.store
            .insert_keypair(&record)
            .await
            .map_err(|error| match error {
                StoreError::KeypairAlreadyExists => ComputeError::Conflict,
                other => ComputeError::Store(other),
            })?;
        Ok(keypair_from_record(record))
    }

    pub async fn create_keypair_for_auth(
        &self,
        auth: &AuthContext,
        name: String,
        public_key: String,
    ) -> Result<Keypair, ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "ImportKeypair").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "ImportKeypair".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("compute", "keypair")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::Unauthorized);
        }
        match self
            .create_keypair(
                auth.principal().id().as_str(),
                auth.effective_scope().id().as_str(),
                name,
                public_key,
            )
            .await
        {
            Ok(kp) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("compute", "keypair").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("compute".to_owned(), "keypair".to_owned())
                        }),
                        ResourceId::new(kp.name.clone()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(kp)
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn list_keypairs_for_auth(
        &self,
        auth: &AuthContext,
    ) -> Result<Vec<Keypair>, ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "ListKeypairs").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "ListKeypairs".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("compute", "keypair")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::Unauthorized);
        }
        self.list_keypairs(
            auth.principal().id().as_str(),
            auth.effective_scope().id().as_str(),
        )
        .await
    }

    pub async fn list_keypairs(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<Keypair>, ComputeError> {
        Ok(self
            .store
            .list_keypairs(user_id, project_id)
            .await?
            .into_iter()
            .map(keypair_from_record)
            .collect())
    }

    pub async fn show_keypair_for_auth(
        &self,
        auth: &AuthContext,
        name: &str,
    ) -> Result<Keypair, ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "ReadKeypair").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "ReadKeypair".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("compute", "keypair")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(name.to_string()).map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::NotFound);
        }
        self.show_keypair(
            auth.principal().id().as_str(),
            auth.effective_scope().id().as_str(),
            name,
        )
        .await
    }

    pub async fn show_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<Keypair, ComputeError> {
        self.store
            .get_keypair(user_id, project_id, name)
            .await
            .map(keypair_from_record)
            .map_err(|error| match error {
                StoreError::KeypairNotFound => ComputeError::NotFound,
                other => ComputeError::Store(other),
            })
    }

    pub async fn delete_keypair_for_auth(
        &self,
        auth: &AuthContext,
        name: &str,
    ) -> Result<(), ComputeError> {
        let ns = ServiceNamespace::new("compute")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("compute".to_owned()));
        let act = ActionId::new("compute", "DeleteKeypair").unwrap_or_else(|_| {
            ActionId::new_unchecked("compute".to_owned(), "DeleteKeypair".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("compute", "keypair")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(name.to_string()).map_err(|_| ComputeError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(ComputeError::NotFound);
        }
        match self
            .delete_keypair(
                auth.principal().id().as_str(),
                auth.effective_scope().id().as_str(),
                name,
            )
            .await
        {
            Ok(()) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("compute", "keypair").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("compute".to_owned(), "keypair".to_owned())
                        }),
                        ResourceId::new(name.to_string()).ok(),
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

    pub async fn delete_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<(), ComputeError> {
        self.store
            .delete_keypair(user_id, project_id, name)
            .await
            .map_err(|error| match error {
                StoreError::KeypairNotFound => ComputeError::NotFound,
                other => ComputeError::Store(other),
            })
    }
}
