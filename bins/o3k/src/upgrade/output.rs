//! Machine output contract for the `upgrade`/`rollback` subcommands
//! (issue #626, plan §1).
//!
//! The JSON object records `source_version`, `target_version`, `phase`,
//! `backup_id`, `status`, `rollback_performed`, and `doctor_status` — and
//! nothing else, so no secrets (credentials, tokens, TLS material) can ever
//! leak through the machine format.

use crate::upgrade::state::UpgradePhase;
use serde::Serialize;

/// Terminal status of one `o3k upgrade` / `o3k rollback` invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeStatus {
    /// The upgrade completed and was committed.
    Committed,
    /// The invocation failed or was blocked (exit 1).
    Failed,
    /// The upgrade failed and a rollback restored the previous release
    /// (exit 1), or `o3k rollback` completed (exit 0).
    RolledBack,
    /// `upgrade --check` passed the read-only preflight (exit 0).
    CheckPassed,
    /// `upgrade --check` was blocked (exit 1).
    CheckBlocked,
}

/// The complete JSON object printed by `upgrade --json` / `rollback --json`.
#[derive(Debug, Clone, Serialize)]
pub struct UpgradeJson {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<UpgradePhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_id: Option<String>,
    pub status: UpgradeStatus,
    pub rollback_performed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doctor_status: Option<String>,
}

impl UpgradeJson {
    /// Builds the JSON view from an engine outcome. The error message is
    /// deliberately not included: it is rendered on stderr for humans and
    /// the machine format stays fixed to the plan §1 field list.
    #[must_use]
    pub fn new(
        source_version: Option<String>,
        target_version: Option<String>,
        phase: Option<UpgradePhase>,
        backup_id: Option<String>,
        status: UpgradeStatus,
        rollback_performed: bool,
        doctor_status: Option<String>,
    ) -> Self {
        Self {
            source_version,
            target_version,
            phase,
            backup_id,
            status,
            rollback_performed,
            doctor_status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JSON shape is exactly the plan §1 field list.
    #[test]
    fn json_shape_matches_the_contract() {
        let json = UpgradeJson::new(
            Some("0.2.0-alpha.2".to_owned()),
            Some("0.3.0-alpha.1".to_owned()),
            Some(UpgradePhase::Committed),
            Some("o3k-upgrade-0.2.0-alpha.2-0.3.0-alpha.1-1712345678".to_owned()),
            UpgradeStatus::Committed,
            false,
            Some("healthy".to_owned()),
        );
        let value = match serde_json::to_value(&json) {
            Ok(value) => value,
            Err(error) => {
                assert!(serde_json::to_value(&json).is_ok(), "{error}");
                return;
            }
        };
        let object = match value.as_object() {
            Some(object) => object,
            None => {
                assert!(value.is_object(), "JSON must be an object");
                return;
            }
        };
        assert_eq!(object.len(), 7, "exactly the plan §1 fields");
        for key in [
            "source_version",
            "target_version",
            "phase",
            "backup_id",
            "status",
            "rollback_performed",
            "doctor_status",
        ] {
            assert!(object.contains_key(key), "missing key {key}");
        }
        assert_eq!(
            object.get("status").and_then(serde_json::Value::as_str),
            Some("committed")
        );
        assert_eq!(
            object.get("phase").and_then(serde_json::Value::as_str),
            Some("COMMITTED")
        );
    }

    /// Optional fields are omitted, never null.
    #[test]
    fn optional_fields_are_omitted() {
        let json = UpgradeJson::new(
            None,
            None,
            None,
            None,
            UpgradeStatus::CheckPassed,
            false,
            None,
        );
        let value = match serde_json::to_value(&json) {
            Ok(value) => value,
            Err(error) => {
                assert!(serde_json::to_value(&json).is_ok(), "{error}");
                return;
            }
        };
        let object = match value.as_object() {
            Some(object) => object,
            None => {
                assert!(value.is_object(), "JSON must be an object");
                return;
            }
        };
        assert_eq!(object.len(), 2, "only status and rollback_performed remain");
        assert_eq!(
            object.get("status").and_then(serde_json::Value::as_str),
            Some("check_passed")
        );
        assert_eq!(
            object.get("rollback_performed"),
            Some(&serde_json::Value::Bool(false))
        );
    }
}
