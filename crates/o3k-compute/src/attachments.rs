use super::{
    AuthContext, ComputeError, ComputeProvider, ComputeService, CreateInstanceRequest, Operation,
    ResourceId, ResourceTarget, ResourceType, ServerId, StoreError, Uuid, VolumeAttachmentRecord,
    durable_inspect_error, provider_error_category_from_name,
};

use o3k_kernel::{ActionId, AuditEvent, AuditOutcome, AuthorizationRequest, ServiceNamespace};

impl ComputeService {
    pub async fn attach_volume_for_auth(
        &self,
        auth: &AuthContext,
        server_id: ServerId,
        volume_id: Uuid,
        device: Option<String>,
        tag: Option<String>,
        delete_on_termination: bool,
    ) -> Result<VolumeAttachmentRecord, ComputeError> {
        let ns = ServiceNamespace::new("volume")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("volume".to_owned()));
        let act = ActionId::new("volume", "AttachVolume").unwrap_or_else(|_| {
            ActionId::new_unchecked("volume".to_owned(), "AttachVolume".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("volume", "volume_attachment")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(server_id.as_uuid().to_string())
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
            return Err(ComputeError::NotFound);
        }
        match self
            .attach_volume(
                auth.effective_scope().id().as_str(),
                server_id,
                volume_id,
                device,
                tag,
                delete_on_termination,
            )
            .await
        {
            Ok(record) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("volume", "volume_attachment").unwrap_or_else(|_| {
                            ResourceType::new_unchecked(
                                "volume".to_owned(),
                                "volume_attachment".to_owned(),
                            )
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

    pub async fn attach_volume(
        &self,
        project_id: &str,
        server_id: ServerId,
        volume_id: Uuid,
        device: Option<String>,
        tag: Option<String>,
        delete_on_termination: bool,
    ) -> Result<VolumeAttachmentRecord, ComputeError> {
        let _ = self.show_server(project_id, server_id).await?;
        self.attachments
            .attach(
                project_id,
                server_id.as_uuid(),
                volume_id,
                device,
                tag,
                delete_on_termination,
            )
            .await
    }

    pub async fn list_volume_attachments_for_auth(
        &self,
        auth: &AuthContext,
        server_id: ServerId,
    ) -> Result<Vec<VolumeAttachmentRecord>, ComputeError> {
        let ns = ServiceNamespace::new("volume")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("volume".to_owned()));
        let act = ActionId::new("volume", "ListVolumeAttachments").unwrap_or_else(|_| {
            ActionId::new_unchecked("volume".to_owned(), "ListVolumeAttachments".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("volume", "volume_attachment")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(server_id.as_uuid().to_string())
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
            return Err(ComputeError::NotFound);
        }
        self.list_volume_attachments(auth.effective_scope().id().as_str(), server_id)
            .await
    }

    pub async fn list_volume_attachments(
        &self,
        project_id: &str,
        server_id: ServerId,
    ) -> Result<Vec<VolumeAttachmentRecord>, ComputeError> {
        let _ = self.show_server(project_id, server_id).await?;
        let records = self
            .store
            .list_volume_attachments(server_id.as_uuid())
            .await?;
        Ok(records
            .into_iter()
            .filter(|r| r.status != "detached")
            .collect())
    }

    pub async fn get_volume_attachment_for_auth(
        &self,
        auth: &AuthContext,
        server_id: ServerId,
        attachment_id: Uuid,
    ) -> Result<VolumeAttachmentRecord, ComputeError> {
        let ns = ServiceNamespace::new("volume")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("volume".to_owned()));
        let act = ActionId::new("volume", "ReadVolumeAttachment").unwrap_or_else(|_| {
            ActionId::new_unchecked("volume".to_owned(), "ReadVolumeAttachment".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("volume", "volume_attachment")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(server_id.as_uuid().to_string())
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
            return Err(ComputeError::NotFound);
        }
        self.get_volume_attachment(
            auth.effective_scope().id().as_str(),
            server_id,
            attachment_id,
        )
        .await
    }

    pub async fn get_volume_attachment(
        &self,
        project_id: &str,
        server_id: ServerId,
        attachment_id: Uuid,
    ) -> Result<VolumeAttachmentRecord, ComputeError> {
        let _ = self.show_server(project_id, server_id).await?;
        self.store
            .get_volume_attachment(server_id.as_uuid(), attachment_id)
            .await?
            .ok_or(ComputeError::NotFound)
    }

    pub async fn detach_volume_for_auth(
        &self,
        auth: &AuthContext,
        server_id: ServerId,
        attachment_id: Uuid,
    ) -> Result<(), ComputeError> {
        let ns = ServiceNamespace::new("volume")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("volume".to_owned()));
        let act = ActionId::new("volume", "DetachVolume").unwrap_or_else(|_| {
            ActionId::new_unchecked("volume".to_owned(), "DetachVolume".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("volume", "volume_attachment")
                    .map_err(|_| ComputeError::InvalidRequest)?,
                ResourceId::new(server_id.as_uuid().to_string())
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
            return Err(ComputeError::NotFound);
        }
        match self
            .detach_volume(
                auth.effective_scope().id().as_str(),
                server_id,
                attachment_id,
            )
            .await
        {
            Ok(()) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("volume", "volume_attachment").unwrap_or_else(|_| {
                            ResourceType::new_unchecked(
                                "volume".to_owned(),
                                "volume_attachment".to_owned(),
                            )
                        }),
                        ResourceId::new(attachment_id.to_string()).ok(),
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

    pub async fn detach_volume(
        &self,
        project_id: &str,
        server_id: ServerId,
        attachment_id: Uuid,
    ) -> Result<(), ComputeError> {
        let _ = self.show_server(project_id, server_id).await?;
        self.attachments
            .detach(project_id, server_id.as_uuid(), attachment_id)
            .await
    }

    /// Revalidates and inspects an already-created server through the
    /// provider boundary. This is deliberately read-only: an existing
    /// Placement allocation is checked, never recreated, before the provider
    /// receives an inspect request.
    pub async fn inspect_server(
        &self,
        project_id: &str,
        id: ServerId,
        idempotency_key: &str,
    ) -> Result<Operation, ComputeError> {
        let resource =
            self.store
                .get_resource(id.as_uuid())
                .await
                .map_err(|error| match error {
                    StoreError::ResourceNotFound => ComputeError::NotFound,
                    other => ComputeError::Store(other),
                })?;
        if resource.project_id != project_id {
            return Err(ComputeError::NotFound);
        }
        let intent: CreateInstanceRequest =
            serde_json::from_str(&resource.desired_state).map_err(|_| ComputeError::Conflict)?;
        let provider_id = intent
            .placement_provider_id
            .as_deref()
            .ok_or(ComputeError::Conflict)?;
        let allocation_id = intent
            .placement_allocation_id
            .as_deref()
            .ok_or(ComputeError::Conflict)?;
        if let Some(scheduler) = &self.scheduler {
            scheduler
                .validate_allocation(provider_id, allocation_id, &id.to_string())
                .await?;
        } else {
            return Err(ComputeError::Conflict);
        }
        let _reference = match self
            .store
            .get_provider_reference(id.as_uuid(), "compute")
            .await
        {
            Ok(reference) => reference,
            Err(StoreError::ProviderReferenceNotFound) => self
                .store
                .get_provider_reference(id.as_uuid(), "compute-agent")
                .await
                .map_err(|error| match error {
                    StoreError::ProviderReferenceNotFound => ComputeError::NotFound,
                    other => ComputeError::Store(other),
                })?,
            Err(other) => return Err(ComputeError::Store(other)),
        };
        if idempotency_key.trim().is_empty() {
            return Err(ComputeError::InvalidRequest);
        }
        let operation_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("o3k:inspect:{project_id}:{id}:{idempotency_key}").as_bytes(),
        );
        let existing = self.store.get_operation(operation_id).await.ok();
        if let Some(record) = existing.as_ref()
            && matches!(
                record.state,
                o3k_store::OperationState::Succeeded | o3k_store::OperationState::Failed
            )
        {
            let state = match record.state {
                o3k_store::OperationState::Succeeded => o3k_provider::OperationState::Succeeded,
                o3k_store::OperationState::Failed => o3k_provider::OperationState::Failed,
                _ => unreachable!("terminal state checked above"),
            };
            return Ok(Operation {
                provider_operation_id: record
                    .provider_operation_id
                    .as_deref()
                    .and_then(|value| Uuid::parse_str(value).ok())
                    .unwrap_or(operation_id),
                o3k_operation_id: operation_id,
                state,
                error_category: record
                    .error_category
                    .as_deref()
                    .and_then(provider_error_category_from_name),
                provider_resource_id: Some(_reference.provider_resource_id.clone()),
            });
        }
        if existing.is_none() {
            self.store
                .insert_operation(&o3k_store::OperationRecord {
                    id: operation_id,
                    resource_id: id.as_uuid(),
                    kind: "inspect".to_owned(),
                    state: o3k_store::OperationState::Pending,
                    provider_operation_id: None,
                    error_category: None,
                    error_message: None,
                })
                .await?;
        }
        let result = self
            .provider
            .inspect_instance(
                provider_id,
                &id.to_string(),
                &_reference.provider_resource_id,
                operation_id,
                idempotency_key,
            )
            .await;
        match result {
            Ok(operation) => {
                let durable_state = match operation.state {
                    o3k_provider::OperationState::Succeeded => o3k_store::OperationState::Succeeded,
                    o3k_provider::OperationState::Failed => o3k_store::OperationState::Failed,
                    o3k_provider::OperationState::UnknownOutcome => {
                        o3k_store::OperationState::UnknownOutcome
                    }
                    _ => o3k_store::OperationState::Running,
                };
                self.store
                    .update_operation(
                        operation_id,
                        durable_state,
                        Some(&operation.provider_operation_id.to_string()),
                        None,
                        None,
                    )
                    .await?;
                Ok(operation)
            }
            Err(error) => {
                let (durable_state, category) = durable_inspect_error(&error);
                self.store
                    .update_operation(
                        operation_id,
                        durable_state,
                        None,
                        Some(category),
                        Some(&error.to_string()),
                    )
                    .await?;
                Err(error.into())
            }
        }
    }
}
