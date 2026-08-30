use std::sync::Arc;

use o3k_kernel::Authorizer;
use o3k_native_api::{error::NativeReadError, network::AddressRealmItem};
use o3k_store::NetworkRepository;
use uuid::Uuid;

use super::helpers::{authorize_collection, authorize_instance};

#[cfg(test)]
fn network_intent_state_wire(state: o3k_domain::NetworkIntentState) -> &'static str {
    match state {
        o3k_domain::NetworkIntentState::Requested => "requested",
        o3k_domain::NetworkIntentState::Active => "active",
        o3k_domain::NetworkIntentState::Deleting => "deleting",
        o3k_domain::NetworkIntentState::Error => "error",
    }
}

// ── NetworkReader ─────────────────────────────────────────────────────────

pub struct NetworkReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
    pub authorizer: Arc<dyn Authorizer>,
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod network_reader_tests {
    use super::{authorize_collection, network_intent_state_wire};

    #[test]
    fn network_intent_state_is_serialized_from_canonical_state() {
        assert_eq!(
            network_intent_state_wire(o3k_domain::NetworkIntentState::Requested),
            "requested"
        );
        assert_eq!(
            network_intent_state_wire(o3k_domain::NetworkIntentState::Deleting),
            "deleting"
        );
        assert_eq!(
            network_intent_state_wire(o3k_domain::NetworkIntentState::Error),
            "error"
        );
    }

    #[test]
    fn denied_canonical_network_action_blocks_matching_scope() {
        let auth = o3k_kernel::AuthContext::new(
            o3k_kernel::Principal::User(o3k_kernel::UserPrincipal::new(
                o3k_kernel::PrincipalId::new_unchecked("user-a"),
                "user-a",
                None,
            )),
            o3k_kernel::OwnershipScope::project(
                o3k_kernel::ScopeId::new_unchecked("project-a"),
                None,
                None,
            ),
            vec!["member".into()],
            1,
            2,
            "audit",
            "request",
            None,
        );
        assert!(!authorize_collection(
            &auth,
            "network:ListAddressRealms",
            "network",
            "address_realm",
            &o3k_kernel::StaticAuthorizer::empty(),
        ));
    }
}

#[async_trait::async_trait]
impl o3k_native_api::network::NetworkReader for NetworkReaderAdapter {
    async fn list_address_realms(
        &self,
        auth: &o3k_kernel::AuthContext,
    ) -> Result<Vec<AddressRealmItem>, NativeReadError> {
        let project_id = auth.effective_scope().id().as_str();
        if !authorize_collection(
            auth,
            "network:ListAddressRealms",
            "network",
            "address_realm",
            self.authorizer.as_ref(),
        ) {
            return Err(NativeReadError::Forbidden);
        }
        let networks = self
            .store
            .list_canonical_networks(project_id)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, project_id = %project_id, "native address realm list failed");
                NativeReadError::Internal
            })?;
        let mut items = Vec::new();
        for network in networks {
            let realms = self
                .store
                .list_canonical_realms(project_id, &network.id)
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, network_id = %network.id, "native address realm list failed");
                    NativeReadError::Internal
                })?;
            for realm in realms {
                items.push(AddressRealmItem {
                    id: realm.id.to_string(),
                    project_id: realm.project_id,
                    prefix: realm.prefix,
                    overlapping_prefixes: realm.overlapping_prefixes,
                    created_at: None,
                    generation: i64::try_from(realm.generation)
                        .map_err(|_| NativeReadError::Internal)?,
                    state: realm.state,
                });
            }
        }
        Ok(items)
    }

    async fn show_address_realm(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: Uuid,
    ) -> Result<AddressRealmItem, NativeReadError> {
        let project_id = auth.effective_scope().id().as_str();
        if !authorize_instance(
            auth,
            "network:ReadAddressRealm",
            "network",
            "address_realm",
            id,
            self.authorizer.as_ref(),
        ) {
            return Err(NativeReadError::Forbidden);
        }
        let networks = self
            .store
            .list_canonical_networks(project_id)
            .await
            .map_err(|_| NativeReadError::Internal)?;
        for network in networks {
            if let Some(realm) = self
                .store
                .list_canonical_realms(project_id, &network.id)
                .await
                .map_err(|_| NativeReadError::Internal)?
                .into_iter()
                .find(|realm| realm.id == id)
            {
                return Ok(AddressRealmItem {
                    id: realm.id.to_string(),
                    project_id: realm.project_id,
                    prefix: realm.prefix,
                    overlapping_prefixes: realm.overlapping_prefixes,
                    created_at: None,
                    generation: i64::try_from(realm.generation)
                        .map_err(|_| NativeReadError::Internal)?,
                    state: realm.state,
                });
            }
        }
        Err(NativeReadError::NotFound)
    }
}
