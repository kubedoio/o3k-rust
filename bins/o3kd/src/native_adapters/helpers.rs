use o3k_kernel::{
    ActionId, AuthorizationRequest, Authorizer, ResourceId, ResourceTarget, ResourceType,
};
use uuid::Uuid;

pub(super) fn authorize_collection(
    auth: &o3k_kernel::AuthContext,
    action: &str,
    namespace: &str,
    name: &str,
    authorizer: &dyn Authorizer,
) -> bool {
    let Ok(action) = ActionId::new(namespace, action.split(':').next_back().unwrap_or(action))
    else {
        return false;
    };
    let Ok(resource_type) = ResourceType::new(namespace, name) else {
        return false;
    };
    authorizer
        .authorize(&AuthorizationRequest {
            auth_context: auth,
            action,
            resource_target: ResourceTarget::collection(
                resource_type,
                Some(auth.effective_scope().id().clone()),
            ),
        })
        .is_allowed()
}

pub(super) fn authorize_instance(
    auth: &o3k_kernel::AuthContext,
    action: &str,
    namespace: &str,
    name: &str,
    id: Uuid,
    authorizer: &dyn Authorizer,
) -> bool {
    let Ok(action) = ActionId::new(namespace, action.split(':').next_back().unwrap_or(action))
    else {
        return false;
    };
    let Ok(resource_type) = ResourceType::new(namespace, name) else {
        return false;
    };
    let Ok(resource_id) = ResourceId::new(id.to_string()) else {
        return false;
    };
    authorizer
        .authorize(&AuthorizationRequest {
            auth_context: auth,
            action,
            resource_target: ResourceTarget::instance(
                resource_type,
                resource_id,
                Some(auth.effective_scope().id().clone()),
            ),
        })
        .is_allowed()
}
