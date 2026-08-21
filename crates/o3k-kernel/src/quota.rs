use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::KernelError;
use crate::registry::ServiceNamespace;
use crate::scope::OwnershipScope;

/// Canonical resource limit key identifying a governed dimension within a service namespace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LimitKey {
    namespace: ServiceNamespace,
    resource: String,
}

impl LimitKey {
    /// Canonical inventory of supported Cloud Kernel quota dimensions.
    pub const KNOWN_DIMENSIONS: &'static [(&'static str, &'static str)] = &[
        ("compute", "servers"),
        ("compute", "vcpus"),
        ("compute", "memory_mb"),
        ("compute", "disk_gb"),
        ("image", "images"),
        ("image", "bytes"),
        ("network", "networks"),
        ("network", "subnets"),
        ("network", "ports"),
        ("network", "address_allocations"),
    ];

    #[must_use]
    pub fn is_known_dimension(namespace: &str, resource: &str) -> bool {
        Self::KNOWN_DIMENSIONS
            .iter()
            .any(|(ns, res)| *ns == namespace && *res == resource)
    }

    #[must_use]
    pub fn is_known(&self) -> bool {
        Self::is_known_dimension(self.namespace.as_str(), &self.resource)
    }

    /// Creates a new validated limit key checked against the canonical registry inventory.
    pub fn new(namespace: &str, resource: &str) -> Result<Self, KernelError> {
        let ns = ServiceNamespace::new(namespace)?;
        let resource_clean = resource.to_ascii_lowercase();
        if resource_clean.is_empty() || resource_clean.len() > 64 {
            return Err(KernelError::InvalidIdentifier(format!(
                "invalid limit resource name: '{resource}'"
            )));
        }
        if !resource_clean
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(KernelError::InvalidIdentifier(format!(
                "limit resource name '{resource}' contains forbidden characters"
            )));
        }
        if !Self::is_known_dimension(ns.as_str(), &resource_clean) {
            return Err(KernelError::InvalidIdentifier(format!(
                "unknown or unregistered quota dimension '{namespace}:{resource}'"
            )));
        }
        Ok(Self {
            namespace: ns,
            resource: resource_clean,
        })
    }

    /// Creates an unchecked limit key.
    #[must_use]
    pub fn new_unchecked(namespace: ServiceNamespace, resource: String) -> Self {
        Self {
            namespace,
            resource: resource.to_ascii_lowercase(),
        }
    }

    #[must_use]
    pub fn namespace(&self) -> &ServiceNamespace {
        &self.namespace
    }

    #[must_use]
    pub fn resource(&self) -> &str {
        &self.resource
    }

    // Well-known standard Compute limit keys
    #[must_use]
    pub fn compute_servers() -> Self {
        Self::new_unchecked(ServiceNamespace::compute(), "servers".to_owned())
    }

    #[must_use]
    pub fn compute_vcpus() -> Self {
        Self::new_unchecked(ServiceNamespace::compute(), "vcpus".to_owned())
    }

    #[must_use]
    pub fn compute_memory_mb() -> Self {
        Self::new_unchecked(ServiceNamespace::compute(), "memory_mb".to_owned())
    }

    #[must_use]
    pub fn compute_disk_gb() -> Self {
        Self::new_unchecked(ServiceNamespace::compute(), "disk_gb".to_owned())
    }

    // Well-known standard Image limit keys
    #[must_use]
    pub fn image_images() -> Self {
        Self::new_unchecked(ServiceNamespace::image(), "images".to_owned())
    }

    #[must_use]
    pub fn image_bytes() -> Self {
        Self::new_unchecked(ServiceNamespace::image(), "bytes".to_owned())
    }

    // Well-known standard Network limit keys
    #[must_use]
    pub fn network_networks() -> Self {
        Self::new_unchecked(ServiceNamespace::network(), "networks".to_owned())
    }

    #[must_use]
    pub fn network_subnets() -> Self {
        Self::new_unchecked(ServiceNamespace::network(), "subnets".to_owned())
    }

    #[must_use]
    pub fn network_ports() -> Self {
        Self::new_unchecked(ServiceNamespace::network(), "ports".to_owned())
    }
}

impl fmt::Display for LimitKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace, self.resource)
    }
}

impl FromStr for LimitKey {
    type Err = KernelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (ns, res) = s.split_once(':').ok_or_else(|| {
            KernelError::InvalidIdentifier(format!(
                "invalid limit key '{s}', expected 'namespace:resource'"
            ))
        })?;
        Self::new(ns, res)
    }
}

/// Canonical limit ceiling for a resource dimension.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum LimitValue {
    /// No maximum ceiling is enforced.
    #[default]
    Unlimited,
    /// A finite maximum ceiling (must not exceed i64::MAX for safe durable storage).
    Maximum(u64),
}

impl LimitValue {
    /// Creates a verified finite maximum limit value.
    pub fn new_maximum_checked(max: u64) -> Result<Self, KernelError> {
        if max > i64::MAX as u64 {
            return Err(KernelError::InvalidIdentifier(format!(
                "limit maximum {max} exceeds maximum supported signed 64-bit integer"
            )));
        }
        Ok(Self::Maximum(max))
    }
}

impl fmt::Display for LimitValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unlimited => write!(f, "unlimited"),
            Self::Maximum(max) => write!(f, "{max}"),
        }
    }
}

/// Quantity of a resource requested or consumed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceAmount {
    pub key: LimitKey,
    pub amount: u64,
}

impl ResourceAmount {
    #[must_use]
    pub fn new(key: LimitKey, amount: u64) -> Self {
        Self { key, amount }
    }

    /// Creates a verified resource amount ensuring bounds fit in signed 64-bit integers.
    pub fn new_checked(key: LimitKey, amount: u64) -> Result<Self, KernelError> {
        if amount > i64::MAX as u64 {
            return Err(KernelError::InvalidIdentifier(format!(
                "resource amount {amount} exceeds maximum supported signed 64-bit integer"
            )));
        }
        Ok(Self { key, amount })
    }

    #[must_use]
    pub fn new_unchecked(key: LimitKey, amount: u64) -> Self {
        Self { key, amount }
    }
}

/// Durable usage observation for a given scope and limit key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub scope: OwnershipScope,
    pub key: LimitKey,
    pub in_use: u64,
    pub reserved: u64,
}

impl Usage {
    #[must_use]
    pub fn new(scope: OwnershipScope, key: LimitKey, in_use: u64, reserved: u64) -> Self {
        Self {
            scope,
            key,
            in_use,
            reserved,
        }
    }

    #[must_use]
    pub fn total_consumed(&self) -> u64 {
        self.in_use.saturating_add(self.reserved)
    }
}

/// Unique identifier for a quota reservation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ReservationId(String);

impl ReservationId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7().to_string())
    }

    pub fn parse(s: impl Into<String>) -> Result<Self, KernelError> {
        let val = s.into();
        if val.is_empty() || val.len() > 128 {
            return Err(KernelError::InvalidIdentifier(
                "invalid reservation id".to_owned(),
            ));
        }
        Ok(Self(val))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ReservationId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ReservationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// State of a quota reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationState {
    /// Reservation holds capacity in-flight while the operation executes.
    Pending,
    /// Operation succeeded and usage is now durably committed.
    Committed,
    /// Operation terminally failed or was cancelled; held capacity is released.
    Released,
}

impl fmt::Display for ReservationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Committed => write!(f, "committed"),
            Self::Released => write!(f, "released"),
        }
    }
}

/// Canonical reservation record correlating quota holds with durable operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub id: ReservationId,
    pub scope: OwnershipScope,
    /// Correlation key matching the existing durable OperationId.
    pub operation_id: String,
    pub amounts: Vec<ResourceAmount>,
    pub state: ReservationState,
    pub created_at: String,
}

impl Reservation {
    #[must_use]
    pub fn new(scope: OwnershipScope, operation_id: String, amounts: Vec<ResourceAmount>) -> Self {
        Self {
            id: ReservationId::new(),
            scope,
            operation_id,
            amounts,
            state: ReservationState::Pending,
            created_at: crate::audit::now_rfc3339(),
        }
    }
}

/// Result of evaluating a requested quota allocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum QuotaDecision {
    Allowed,
    Denied {
        key: LimitKey,
        limit: LimitValue,
        used: u64,
        requested: u64,
        reason: String,
    },
}

impl QuotaDecision {
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }

    /// Evaluates whether a single resource amount fits within the given limit and usage.
    #[must_use]
    pub fn evaluate(key: &LimitKey, limit: LimitValue, current_used: u64, requested: u64) -> Self {
        match limit {
            LimitValue::Unlimited => Self::Allowed,
            LimitValue::Maximum(max) => {
                let total = current_used.saturating_add(requested);
                if total <= max {
                    Self::Allowed
                } else {
                    Self::Denied {
                        key: key.clone(),
                        limit,
                        used: current_used,
                        requested,
                        reason: format!(
                            "Quota exceeded for '{key}': limit {max}, used {current_used}, requested {requested}"
                        ),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_key_parsing_and_display() -> Result<(), crate::KernelError> {
        let key = LimitKey::new("compute", "servers")?;
        assert_eq!(key.to_string(), "compute:servers");

        let parsed: LimitKey = "compute:servers".parse()?;
        assert_eq!(parsed, key);

        assert!(LimitKey::new("", "servers").is_err());
        assert!(LimitKey::new("compute", "").is_err());
        assert!(LimitKey::new("compute", "bad/char").is_err());
        // Unknown or unregistered keys fail closed
        assert!(LimitKey::new("compute", "servres").is_err());
        assert!(LimitKey::new("network", "foo").is_err());
        assert!(LimitKey::new("unknown", "anything").is_err());
        Ok(())
    }

    #[test]
    fn numeric_bounds_enforcement() -> Result<(), crate::KernelError> {
        let key = LimitKey::compute_servers();

        // Safe bounds within i64::MAX
        assert!(LimitValue::new_maximum_checked(i64::MAX as u64).is_ok());
        assert!(ResourceAmount::new_checked(key.clone(), i64::MAX as u64).is_ok());

        // Overflowing i64::MAX is rejected fail-closed
        assert!(LimitValue::new_maximum_checked((i64::MAX as u64) + 1).is_err());
        assert!(LimitValue::new_maximum_checked(u64::MAX).is_err());
        assert!(ResourceAmount::new_checked(key.clone(), (i64::MAX as u64) + 1).is_err());
        assert!(ResourceAmount::new_checked(key, u64::MAX).is_err());

        Ok(())
    }

    #[test]
    fn all_known_dimensions_parse_and_validate() {
        for (ns, res) in LimitKey::KNOWN_DIMENSIONS {
            let key = LimitKey::new(ns, res);
            assert!(key.is_ok(), "failed to construct valid key {ns}:{res}");
            let key = key.unwrap_or_else(|_| LimitKey::compute_servers());
            assert_eq!(key.namespace().as_str(), *ns);
            assert_eq!(key.resource(), *res);
        }
    }

    #[test]
    fn quota_decision_evaluation() {
        let key = LimitKey::compute_servers();

        // Unlimited always allows
        assert_eq!(
            QuotaDecision::evaluate(&key, LimitValue::Unlimited, 100, 50),
            QuotaDecision::Allowed
        );

        // Maximum allows within limit
        assert_eq!(
            QuotaDecision::evaluate(&key, LimitValue::Maximum(10), 5, 5),
            QuotaDecision::Allowed
        );

        // Maximum denies exceedance
        let decision = QuotaDecision::evaluate(&key, LimitValue::Maximum(10), 8, 3);
        assert!(matches!(
            decision,
            QuotaDecision::Denied {
                key: k,
                limit: LimitValue::Maximum(10),
                used: 8,
                requested: 3,
                ..
            } if k == key
        ));
    }

    #[test]
    fn reservation_lifecycle() {
        let scope =
            OwnershipScope::project(crate::scope::ScopeId::new_unchecked("proj-1"), None, None);
        let mut res = Reservation::new(
            scope.clone(),
            "op-123".to_owned(),
            vec![ResourceAmount::new_unchecked(
                LimitKey::compute_servers(),
                1,
            )],
        );
        assert_eq!(res.state, ReservationState::Pending);
        assert_eq!(res.operation_id, "op-123");

        res.state = ReservationState::Committed;
        assert_eq!(res.state, ReservationState::Committed);

        res.state = ReservationState::Released;
        assert_eq!(res.state, ReservationState::Released);
    }
}
