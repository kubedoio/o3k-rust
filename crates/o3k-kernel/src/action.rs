use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::KernelError;

/// Stable typed action identifier for protected operations.
///
/// Action IDs follow the `namespace:Action` naming convention (e.g. `compute:CreateServer`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionId {
    namespace: String,
    action: String,
}

impl ActionId {
    /// Constructs and validates a new `ActionId`.
    pub fn new(
        namespace: impl Into<String>,
        action: impl Into<String>,
    ) -> Result<Self, KernelError> {
        let ns = namespace.into();
        let act = action.into();
        let ns_trimmed = ns.trim();
        let act_trimmed = act.trim();
        if ns_trimmed.is_empty() || act_trimmed.is_empty() {
            return Err(KernelError::InvalidActionId(
                "action namespace and action name must not be empty".to_string(),
            ));
        }
        Ok(Self {
            namespace: ns_trimmed.to_lowercase(),
            action: act_trimmed.to_string(),
        })
    }

    /// Creates an `ActionId` without validation.
    #[must_use]
    pub fn new_unchecked(namespace: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            action: action.into(),
        }
    }

    /// Parses an action string of the form `namespace:Action`.
    pub fn parse(s: &str) -> Result<Self, KernelError> {
        let parts: Vec<&str> = s.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(KernelError::InvalidActionId(format!(
                "action id must be of the form 'namespace:Action', got {s:?}"
            )));
        }
        Self::new(parts[0], parts[1])
    }

    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    #[must_use]
    pub fn action(&self) -> &str {
        &self.action
    }

    #[must_use]
    pub fn as_str(&self) -> String {
        format!("{}:{}", self.namespace, self.action)
    }
}

impl fmt::Display for ActionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace, self.action)
    }
}
