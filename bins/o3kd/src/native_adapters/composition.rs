use std::sync::Arc;
use std::time::SystemTime;

use o3k_native_api::resource::{ResourceApplication, ResourceApplicationError};
use o3k_store::DurableStore;
use uuid::Uuid;

/// Store-backed canonical operation visibility adapter. Historical operation
/// rows without P12.4 metadata fail closed rather than being reconstructed
/// with fabricated ownership or action fields.
pub struct CompositionResourceHandler {
    pub application: Arc<dyn ResourceApplication>,
    pub store: Arc<o3k_store::unified::O3kStore>,
    pub manifests: Arc<o3k_kernel::ManifestRegistry>,
    pub delegation_keys: std::collections::HashMap<String, ed25519_dalek::VerifyingKey>,
    pub dispatcher: o3k_native_api::resource::ResourceDispatcher,
}

impl CompositionResourceHandler {
    async fn validate_relationship(
        &self,
        parent_id: Uuid,
        request: &o3k_service_sdk::composition::ChildResourceRequest,
        child: &o3k_kernel::ResourceReference,
        require_exclusive: bool,
    ) -> Result<o3k_store::ResourceRelationshipRecord, o3k_service_sdk::composition::CompositionError>
    {
        let relationship = self
            .store
            .get_relationship(parent_id, &request.slot)
            .await
            .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        let child_id = child
            .resource_id
            .as_str()
            .parse::<Uuid>()
            .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        if relationship.parent_resource_type != request.parent.resource_type.to_string()
            || relationship.expected_child_resource_type != child.resource_type.to_string()
            || relationship.child_resource_id != Some(child_id)
            || relationship.parent_operation_id != request.parent_operation_id
            || relationship.owner_scope != request.owner_scope.id().as_str()
            || matches!(relationship.state.as_str(), "reserved" | "deleted")
            || (require_exclusive && relationship.ownership != "exclusive")
        {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        if let Some(child_operation_id) = request.child_operation_id
            && relationship.child_operation_id != Some(child_operation_id)
        {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        Ok(relationship)
    }

    async fn validate_parent(
        &self,
        request: &o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<Uuid, o3k_service_sdk::composition::CompositionError> {
        let parent_id: Uuid = request
            .parent
            .resource_id
            .as_str()
            .parse()
            .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        let parent = self
            .store
            .get_resource(parent_id)
            .await
            .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        if parent.kind != request.parent.resource_type.to_string()
            || parent.project_id != request.owner_scope.id().as_str()
        {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        let operation = self
            .store
            .get_operation(request.parent_operation_id)
            .await
            .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        let canonical = self
            .store
            .get_canonical_operation(request.parent_operation_id)
            .await
            .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        if operation.resource_id != parent_id
            || canonical.resource_id.as_deref() != Some(parent_id.to_string().as_str())
            || canonical.service != request.context.service_id
            || canonical.action != request.context.action.to_string()
            || canonical.owner_scope != request.owner_scope.id().as_str()
        {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        Ok(parent_id)
    }

    fn authenticate(
        &self,
        request: &o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<o3k_kernel::AuthContext, o3k_service_sdk::composition::CompositionError> {
        let claims = o3k_service_sdk::verify_wire_delegation(
            &request.delegation,
            &self.delegation_keys,
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?
                .as_millis() as u64,
        )
        .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        if claims.original_actor.trim().is_empty()
            || claims.owner_scope != request.owner_scope.to_string()
            || claims.operation_id != request.parent_operation_id
        {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        let (kind, id) = claims
            .owner_scope
            .split_once(':')
            .ok_or(o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        if kind != "project" {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        let scope = o3k_kernel::OwnershipScope::project(
            o3k_kernel::ScopeId::new(id)
                .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?,
            None,
            None,
        );
        let request_id = claims.request_id.to_string();
        Ok(o3k_kernel::AuthContext::new(
            o3k_kernel::Principal::User(o3k_kernel::UserPrincipal::new(
                o3k_kernel::PrincipalId::new(claims.original_actor.clone())
                    .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?,
                claims.original_actor,
                None,
            )),
            scope,
            Vec::new(),
            claims.issued_at_unix_ms / 1000,
            claims.expires_at_unix_ms / 1000,
            request.context.audit_correlation.clone(),
            request_id,
            Some(o3k_kernel::ServicePrincipal::new(
                o3k_kernel::PrincipalId::new(request.service_principal.clone())
                    .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?,
                request.service_principal.clone(),
                request.context.service_id.clone(),
            )),
        ))
    }

    fn dependency_allowed(
        &self,
        request: &o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<(), o3k_service_sdk::composition::CompositionError> {
        let Some(manifest) = self.manifests.get(&request.context.service_id) else {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        };
        let expected_principal = manifest
            .controller
            .as_ref()
            .and_then(|controller| controller.service_principal.as_deref())
            .ok_or(o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        if expected_principal != request.service_principal
            || request.parent.resource_type.namespace() != manifest.namespace
        {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        let resource = request.resource_type.to_string();
        let action = request.action.to_string();
        let declared = manifest.dependencies.iter().any(|dependency| {
            (dependency.kind == o3k_kernel::manifest::DependencyKind::ResourceType
                && dependency.name == resource)
                || (dependency.kind == o3k_kernel::manifest::DependencyKind::Action
                    && dependency.name == action)
        });
        if declared && self.manifests.has_action(&request.action) {
            Ok(())
        } else {
            Err(o3k_service_sdk::composition::CompositionError::Unauthorized)
        }
    }

    fn relationship_record(
        &self,
        request: &o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<o3k_store::ResourceRelationshipRecord, o3k_service_sdk::composition::CompositionError>
    {
        Ok(o3k_store::ResourceRelationshipRecord {
            parent_resource_id: request
                .parent
                .resource_id
                .as_str()
                .parse()
                .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?,
            parent_resource_type: request.parent.resource_type.to_string(),
            slot: request.slot.clone(),
            expected_child_resource_type: request.resource_type.to_string(),
            child_resource_id: request
                .child
                .as_ref()
                .and_then(|child| child.resource_id.as_str().parse().ok()),
            ownership: "exclusive".to_owned(),
            parent_operation_id: request.parent_operation_id,
            child_operation_id: request.child_operation_id,
            owner_scope: request.owner_scope.id().as_str().to_owned(),
            state: "reserved".to_owned(),
            fingerprint: request.context.replay_identity.clone(),
        })
    }

    fn descriptor_for(
        &self,
        resource_type: &o3k_kernel::ResourceType,
    ) -> Result<
        o3k_native_api::resource::ResourceDescriptor,
        o3k_service_sdk::composition::CompositionError,
    > {
        self.dispatcher
            .resolve_resource_type(resource_type)
            .cloned()
            .ok_or_else(|| {
                o3k_service_sdk::composition::CompositionError::Failed(format!(
                    "child resource is not registered: {resource_type}"
                ))
            })
    }
}

#[async_trait::async_trait]
impl o3k_service_sdk::composition::CompositionHandler for CompositionResourceHandler {
    async fn create_child(
        &self,
        request: o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<
        o3k_service_sdk::composition::ChildResourceReceipt,
        o3k_service_sdk::composition::CompositionError,
    > {
        self.dependency_allowed(&request).map_err(|_| {
            o3k_service_sdk::composition::CompositionError::Failed(format!(
                "dependency denied for {}",
                request.action
            ))
        })?;
        let auth = self.authenticate(&request).map_err(|_| {
            o3k_service_sdk::composition::CompositionError::Failed("delegation denied".into())
        })?;
        let parent_id = self.validate_parent(&request).await.map_err(|_| {
            o3k_service_sdk::composition::CompositionError::Failed("parent denied".into())
        })?;
        // Child creation is an allocation operation, not an adoption API. A
        // caller-supplied canonical child ID could otherwise be recorded in a
        // relationship and then silently ignored by the create path. Reject
        // it before reserving a relationship or invoking the child service.
        if request.child.is_some() {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        let descriptor = self.descriptor_for(&request.resource_type)?;
        let expected_action = descriptor
            .lifecycle_actions
            .get(&o3k_native_api::resource::LifecycleOperation::Create)
            .ok_or(o3k_service_sdk::composition::CompositionError::Failed(
                "child create operation is not declared".into(),
            ))?;
        if expected_action != &request.action {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        let relationship = self
            .store
            .reserve_relationship(&self.relationship_record(&request)?)
            .await
            .map_err(|_| {
                o3k_service_sdk::composition::CompositionError::Failed(
                    "relationship reservation failed".into(),
                )
            })?;
        // A durable relationship intent is not an empty slot. If a previous
        // create has an operation identity, or the slot is already uncertain
        // or deleting, recovery must observe the canonical operation before
        // another mutation can be attempted.
        if relationship.child_resource_id.is_none()
            && (relationship.child_operation_id.is_some()
                || matches!(relationship.state.as_str(), "unknown" | "deleting"))
        {
            return Err(o3k_service_sdk::composition::CompositionError::UnknownOutcome);
        }
        if let (Some(child), Some(operation_id)) = (
            relationship.child_resource_id,
            relationship.child_operation_id,
        ) {
            return Ok(o3k_service_sdk::composition::ChildResourceReceipt {
                resource: o3k_kernel::ResourceReference {
                    resource_type: request.resource_type,
                    resource_id: o3k_kernel::ResourceId::new(child.to_string()).map_err(|_| {
                        o3k_service_sdk::composition::CompositionError::Failed(
                            "invalid child id".into(),
                        )
                    })?,
                    generation: 1,
                },
                operation_id,
                owner_scope: request.owner_scope,
                ownership: o3k_kernel::RelationshipOwnership::Exclusive,
            });
        }
        let result = self
            .application
            .create(
                &descriptor,
                &auth,
                o3k_native_api::resource::CreateRequest {
                    api_version: Some("o3k.io/v1".into()),
                    kind: Some(request.resource_type.to_string()),
                    spec: request.desired_spec,
                },
                Some(&request.idempotency_key),
            )
            .await
            .map_err(|_| {
                o3k_service_sdk::composition::CompositionError::Failed("child create failed".into())
            })?;
        let child_id = result
            .resource_id
            .ok_or(o3k_service_sdk::composition::CompositionError::UnknownOutcome)?
            .parse()
            .map_err(|_| o3k_service_sdk::composition::CompositionError::UnknownOutcome)?;
        let child_operation_id = result
            .operation_id
            .parse()
            .map_err(|_| o3k_service_sdk::composition::CompositionError::UnknownOutcome)?;
        let bound = self
            .store
            .bind_relationship(parent_id, &request.slot, child_id, child_operation_id)
            .await
            .map_err(|_| {
                o3k_service_sdk::composition::CompositionError::Failed(
                    "relationship bind failed".into(),
                )
            })?;
        Ok(o3k_service_sdk::composition::ChildResourceReceipt {
            resource: o3k_kernel::ResourceReference {
                resource_type: request.resource_type,
                resource_id: o3k_kernel::ResourceId::new(child_id.to_string()).map_err(|_| {
                    o3k_service_sdk::composition::CompositionError::Failed(
                        "invalid child id".into(),
                    )
                })?,
                generation: 1,
            },
            operation_id: bound.child_operation_id.unwrap_or(child_operation_id),
            owner_scope: request.owner_scope,
            ownership: o3k_kernel::RelationshipOwnership::Exclusive,
        })
    }

    async fn observe_child(
        &self,
        request: o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<serde_json::Value, o3k_service_sdk::composition::CompositionError> {
        let auth = self.authenticate(&request)?;
        let parent_id = self.validate_parent(&request).await?;
        let child =
            request
                .child
                .clone()
                .ok_or(o3k_service_sdk::composition::CompositionError::Failed(
                    "missing child reference".into(),
                ))?;
        let descriptor = self.descriptor_for(&child.resource_type)?;
        let expected_action = descriptor
            .lifecycle_actions
            .get(&o3k_native_api::resource::LifecycleOperation::Show)
            .ok_or(o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        let manifest = self
            .manifests
            .get(&request.context.service_id)
            .ok_or(o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        if !manifest.dependencies.iter().any(|dependency| {
            dependency.kind == o3k_kernel::manifest::DependencyKind::Action
                && dependency.name == expected_action.to_string()
        }) {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        self.validate_relationship(parent_id, &request, &child, false)
            .await?;
        self.application
            .show(&descriptor, &auth, child.resource_id.as_str())
            .await
            .map_err(|error| {
                o3k_service_sdk::composition::CompositionError::Failed(format!(
                    "child observation failed for {} {}: {error:?}",
                    child.resource_type, child.resource_id
                ))
            })
    }

    async fn delete_child(
        &self,
        request: o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<
        o3k_service_sdk::composition::ChildResourceReceipt,
        o3k_service_sdk::composition::CompositionError,
    > {
        self.dependency_allowed(&request).map_err(|_| {
            o3k_service_sdk::composition::CompositionError::Failed(format!(
                "delete dependency denied for {}",
                request.action
            ))
        })?;
        let auth = self.authenticate(&request).map_err(|_| {
            o3k_service_sdk::composition::CompositionError::Failed(
                "delete delegation denied".into(),
            )
        })?;
        let parent_id = self.validate_parent(&request).await.map_err(|_| {
            o3k_service_sdk::composition::CompositionError::Failed("delete parent denied".into())
        })?;
        let child = request.child.clone().ok_or_else(|| {
            o3k_service_sdk::composition::CompositionError::Failed("delete child missing".into())
        })?;
        self.validate_relationship(parent_id, &request, &child, true)
            .await?;
        let descriptor = self.descriptor_for(&child.resource_type)?;
        let expected_action = descriptor
            .lifecycle_actions
            .get(&o3k_native_api::resource::LifecycleOperation::Delete)
            .ok_or(o3k_service_sdk::composition::CompositionError::Failed(
                "child delete operation is not declared".into(),
            ))?;
        if expected_action != &request.action {
            return Err(o3k_service_sdk::composition::CompositionError::Failed(
                format!(
                    "delete action mismatch expected={} actual={}",
                    expected_action, request.action
                ),
            ));
        }
        self.store
            .set_relationship_state(parent_id, &request.slot, "deleting")
            .await
            .map_err(|_| {
                o3k_service_sdk::composition::CompositionError::Failed(
                    "relationship state update failed".into(),
                )
            })?;
        let result = self
            .application
            .delete(
                &descriptor,
                &auth,
                child.resource_id.as_str(),
                Some(&request.idempotency_key),
                None,
            )
            .await
            .map_err(|_| {
                o3k_service_sdk::composition::CompositionError::Failed("child delete failed".into())
            })?;
        if !result.complete {
            // An accepted child delete is not proof of absence.  A read that
            // proves NotFound is the only safe fast path; every other result
            // remains recoverable as unknown.
            match self
                .application
                .show(&descriptor, &auth, child.resource_id.as_str())
                .await
            {
                Err(ResourceApplicationError::NotFound) => {}
                _ => {
                    self.store
                        .set_relationship_state(parent_id, &request.slot, "unknown")
                        .await
                        .map_err(|_| {
                            o3k_service_sdk::composition::CompositionError::Failed(
                                "relationship state update failed".into(),
                            )
                        })?;
                    return Err(o3k_service_sdk::composition::CompositionError::UnknownOutcome);
                }
            }
        }
        self.store
            .set_relationship_state(parent_id, &request.slot, "deleted")
            .await
            .map_err(|_| {
                o3k_service_sdk::composition::CompositionError::Failed(
                    "relationship state update failed".into(),
                )
            })?;
        Ok(o3k_service_sdk::composition::ChildResourceReceipt {
            resource: child,
            operation_id: result
                .operation_id
                .parse()
                .map_err(|_| o3k_service_sdk::composition::CompositionError::UnknownOutcome)?,
            owner_scope: request.owner_scope,
            ownership: o3k_kernel::RelationshipOwnership::Exclusive,
        })
    }

    async fn list_relationships(
        &self,
        request: o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<
        Vec<o3k_service_sdk::composition::RelationshipView>,
        o3k_service_sdk::composition::CompositionError,
    > {
        self.authenticate(&request)?;
        self.validate_parent(&request).await?;
        let parent = request
            .parent
            .resource_id
            .as_str()
            .parse()
            .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        let records = self.store.list_relationships(parent).await.map_err(|_| {
            o3k_service_sdk::composition::CompositionError::Failed(
                "relationship listing failed".into(),
            )
        })?;
        records
            .into_iter()
            .map(|record| {
                let (namespace, name) = record
                    .expected_child_resource_type
                    .split_once(':')
                    .ok_or_else(|| {
                        o3k_service_sdk::composition::CompositionError::Failed(
                            "invalid relationship resource type".into(),
                        )
                    })?;
                let resource_type =
                    o3k_kernel::ResourceType::new(namespace, name).map_err(|_| {
                        o3k_service_sdk::composition::CompositionError::Failed(
                            "invalid relationship resource type".into(),
                        )
                    })?;
                let resource = record
                    .child_resource_id
                    .map(|id| {
                        Ok::<_, o3k_service_sdk::composition::CompositionError>(
                            o3k_kernel::ResourceReference {
                                resource_type: resource_type.clone(),
                                resource_id: o3k_kernel::ResourceId::new(id.to_string()).map_err(
                                    |_| {
                                        o3k_service_sdk::composition::CompositionError::Failed(
                                            "invalid relationship resource id".into(),
                                        )
                                    },
                                )?,
                                generation: 1,
                            },
                        )
                    })
                    .transpose()?;
                Ok(o3k_service_sdk::composition::RelationshipView {
                    slot: record.slot,
                    resource,
                    resource_type,
                    ownership: if record.ownership == "referenced" {
                        o3k_kernel::RelationshipOwnership::Referenced
                    } else {
                        o3k_kernel::RelationshipOwnership::Exclusive
                    },
                    state: record.state,
                    parent_operation_id: record.parent_operation_id,
                    child_operation_id: record.child_operation_id,
                })
            })
            .collect()
    }
}
