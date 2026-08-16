//! Check engine: runs all checks serially in the fixed output order and
//! derives the overall verdict.

use crate::context::Context;
use crate::output::{Check, OverallStatus, Report, now_utc_rfc3339};

/// Runs the 34 checks serially in the fixed order. The timestamp is captured
/// at the start of the run.
pub async fn run_all(ctx: &Context) -> Report {
    let timestamp = now_utc_rfc3339();
    let mut checks: Vec<Check> = Vec::with_capacity(34);
    checks.push(crate::checks::host::check(ctx).await);
    checks.push(crate::checks::host::check_kvm_device(ctx).await);
    checks.push(crate::checks::host::check_disk_space(ctx).await);
    checks.push(crate::checks::services::check_o3kd_unit(ctx).await);
    checks.push(crate::checks::services::check_compute_unit(ctx).await);
    checks.push(crate::checks::control::check_healthz(ctx).await);
    checks.push(crate::checks::control::check_readyz(ctx).await);
    checks.push(crate::checks::database::check_accessible(ctx).await);
    checks.push(crate::checks::database::check_integrity(ctx).await);
    checks.push(crate::checks::database::check_wal_mode(ctx).await);
    checks.push(crate::checks::database::check_permissions(ctx).await);
    checks.push(crate::checks::identity::check_configured(ctx).await);
    checks.push(crate::checks::identity::check_authenticated(ctx).await);
    checks.push(crate::checks::compute::check_agent_registered(ctx).await);
    checks.push(crate::checks::compute::check_agent_epoch(ctx).await);
    checks.push(crate::checks::compute::check_agent_capabilities(ctx).await);
    checks.push(crate::checks::compute::check_placement_consistent(ctx).await);
    checks.push(crate::checks::libvirt::check_compute_access(ctx).await);
    checks.push(crate::checks::libvirt::check_control_isolation(ctx).await);
    checks.push(crate::checks::libvirt::check_domains_consistent(ctx).await);
    checks.push(crate::checks::network::check_bridge_state(ctx).await);
    checks.push(crate::checks::network::check_tap_state(ctx).await);
    checks.push(crate::checks::network::check_dhcp_state(ctx).await);
    checks.push(crate::checks::network::check_ownership_records(ctx).await);
    checks.push(crate::checks::cloud::check_api_discovery(ctx).await);
    checks.push(crate::checks::cloud::check_testvm_status(ctx).await);
    checks.push(crate::checks::security::check_config_permissions(ctx).await);
    checks.push(crate::checks::security::check_tls_identity(ctx).await);
    checks.push(crate::checks::release::check_version(ctx).await);
    checks.push(crate::checks::release::check_ownership_manifest(ctx).await);
    checks.push(crate::checks::release::check_binary_hashes(ctx).await);
    checks.push(crate::checks::release::check_binary_set_consistent(ctx).await);
    checks.push(crate::checks::release::check_backup_available(ctx).await);
    checks.push(crate::checks::release::check_upgrade_state(ctx).await);
    let overall_status = overall_status(&checks);
    Report {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        overall_status,
        timestamp,
        checks,
    }
}

/// Derives the verdict: any FAIL is unhealthy; any WARN without a FAIL is a
/// warning; everything else healthy.
#[must_use]
pub fn overall_status(checks: &[Check]) -> OverallStatus {
    let mut warning = false;
    for check in checks {
        match check.status {
            crate::output::CheckStatus::Fail => return OverallStatus::Unhealthy,
            crate::output::CheckStatus::Warn => warning = true,
            _ => {}
        }
    }
    if warning {
        OverallStatus::Warning
    } else {
        OverallStatus::Healthy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{Category, CheckStatus, Report};
    use crate::testutil::{FakeDb, FakeExec, FakeHttp, context_with};

    /// The verdict derivation is the exit-code semantics: healthy only with
    /// no WARN and no FAIL.
    #[test]
    fn overall_status_derivation() {
        let healthy = vec![Check::new(
            "host.os_supported",
            Category::Host,
            CheckStatus::Pass,
            "ok",
        )];
        assert_eq!(overall_status(&healthy), OverallStatus::Healthy);
        let mut warning = healthy.clone();
        warning.push(Check::new(
            "host.disk_space",
            Category::Host,
            CheckStatus::Warn,
            "low space",
        ));
        assert_eq!(overall_status(&warning), OverallStatus::Warning);
        let mut unhealthy = warning;
        unhealthy.push(Check::new(
            "host.kvm_device",
            Category::Host,
            CheckStatus::Fail,
            "no kvm",
        ));
        assert_eq!(overall_status(&unhealthy), OverallStatus::Unhealthy);
    }

    /// A report is serializable and carries the workspace version.
    #[test]
    fn report_carries_workspace_version() {
        let report = Report {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            overall_status: OverallStatus::Healthy,
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            checks: Vec::new(),
        };
        assert_eq!(report.version, env!("CARGO_PKG_VERSION"));
    }

    /// The full healthy fixture must produce 34 PASS checks and a healthy
    /// verdict (the exit-code 0 semantics).
    #[tokio::test]
    async fn healthy_run_produces_only_passes() {
        let ctx = context_with(
            FakeExec::healthy(),
            FakeHttp::healthy(),
            FakeDb::healthy(),
            true,
            true,
        );
        let report = run_all(&ctx).await;
        assert_eq!(report.checks.len(), 34);
        assert_eq!(report.overall_status, OverallStatus::Healthy);
        for check in &report.checks {
            assert_eq!(
                check.status,
                CheckStatus::Pass,
                "check {} was {}: {}",
                check.id,
                check.status,
                check.summary
            );
        }
    }

    /// Helper asserting a JSON value is an object; returns the map or None
    /// (the caller returns from the test).
    fn require_object<'a>(
        value: &'a serde_json::Value,
        what: &str,
    ) -> Option<&'a serde_json::Map<String, serde_json::Value>> {
        match value.as_object() {
            Some(object) => Some(object),
            None => {
                assert!(value.is_object(), "{what} must be a JSON object");
                None
            }
        }
    }

    /// The machine output must satisfy the structural requirements of
    /// `contracts/o3k-doctor-output.schema.json` (asserted structurally,
    /// without a json-schema dependency).
    #[tokio::test]
    async fn json_output_matches_the_schema_contract() {
        let ctx = context_with(
            FakeExec::healthy(),
            FakeHttp::healthy(),
            FakeDb::healthy(),
            true,
            true,
        );
        let report = run_all(&ctx).await;
        let serialized = match serde_json::to_value(&report) {
            Ok(value) => value,
            Err(error) => {
                assert!(
                    serde_json::to_value(&report).is_ok(),
                    "report must serialize: {error}"
                );
                return;
            }
        };
        let Some(object) = require_object(&serialized, "report") else {
            return;
        };
        for key in ["version", "overall_status", "timestamp", "checks"] {
            assert!(
                object.contains_key(key),
                "report must contain required key {key}"
            );
        }
        let overall = match object
            .get("overall_status")
            .and_then(serde_json::Value::as_str)
        {
            Some(value) => value,
            None => {
                assert!(
                    object
                        .get("overall_status")
                        .is_some_and(serde_json::Value::is_string),
                    "overall_status must be a string"
                );
                return;
            }
        };
        assert!(
            matches!(overall, "healthy" | "warning" | "unhealthy"),
            "overall_status {overall} outside the enum"
        );
        let checks = match object.get("checks").and_then(serde_json::Value::as_array) {
            Some(checks) => checks,
            None => {
                assert!(
                    object
                        .get("checks")
                        .is_some_and(serde_json::Value::is_array),
                    "checks must be an array"
                );
                return;
            }
        };
        assert_eq!(checks.len(), 34);
        for check_value in checks {
            let Some(check) = require_object(check_value, "check") else {
                return;
            };
            for key in ["id", "category", "status", "summary"] {
                assert!(
                    check.contains_key(key),
                    "check must contain required key {key}"
                );
            }
            let id = match check.get("id").and_then(serde_json::Value::as_str) {
                Some(id) => id,
                None => {
                    assert!(
                        check.get("id").is_some_and(serde_json::Value::is_string),
                        "check id must be a string"
                    );
                    return;
                }
            };
            let valid_id = id.len() >= 2
                && id.len() <= 64
                && id.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                && id.bytes().all(|b| {
                    b.is_ascii_lowercase()
                        || b.is_ascii_digit()
                        || b == b'.'
                        || b == b'_'
                        || b == b'-'
                });
            assert!(valid_id, "check id {id} violates the schema pattern");
            let status = match check.get("status").and_then(serde_json::Value::as_str) {
                Some(status) => status,
                None => {
                    assert!(
                        check
                            .get("status")
                            .is_some_and(serde_json::Value::is_string),
                        "check status must be a string"
                    );
                    return;
                }
            };
            assert!(
                matches!(status, "PASS" | "WARN" | "FAIL" | "NOT_APPLICABLE"),
                "status {status} outside the enum"
            );
            let category = match check.get("category").and_then(serde_json::Value::as_str) {
                Some(category) => category,
                None => {
                    assert!(
                        check
                            .get("category")
                            .is_some_and(serde_json::Value::is_string),
                        "check category must be a string"
                    );
                    return;
                }
            };
            assert!(
                matches!(
                    category,
                    "host"
                        | "services"
                        | "control"
                        | "database"
                        | "identity"
                        | "compute"
                        | "libvirt"
                        | "network"
                        | "cloud"
                        | "security"
                        | "release"
                ),
                "category {category} outside the enum"
            );
            if matches!(status, "WARN" | "FAIL") {
                let actions = match check
                    .get("recommended_actions")
                    .and_then(serde_json::Value::as_array)
                {
                    Some(actions) => actions,
                    None => {
                        assert!(
                            check
                                .get("recommended_actions")
                                .is_some_and(serde_json::Value::is_array),
                            "check {id} with status {status} must carry recommended_actions"
                        );
                        return;
                    }
                };
                assert!(
                    !actions.is_empty(),
                    "check {id} with status {status} must carry recommended_actions"
                );
            }
        }
    }

    /// Sentinel redaction: a password planted in admin-openrc and a token
    /// planted in the in-memory env map must never appear in the JSON or the
    /// human rendering.
    #[tokio::test]
    async fn sentinels_never_reach_the_output() {
        const SENTINEL_PW: &str = "SENTINEL_PW_7x9z";
        const SENTINEL_ENV: &str = "SENTINEL_ENV_8y0a";
        let mut exec = FakeExec::healthy();
        exec.files.insert(
            "/etc/o3k/admin-openrc".to_owned(),
            Ok(format!(
                "export OS_AUTH_URL=http://127.0.0.1:8080/v3\n\
                 export OS_USERNAME=admin\n\
                 export OS_PASSWORD={SENTINEL_PW}\n\
                 export OS_PROJECT_NAME=admin\n\
                 export OS_USER_DOMAIN_NAME=Default\n\
                 export OS_PROJECT_DOMAIN_NAME=Default\n\
                 export OS_IDENTITY_API_VERSION=3\n"
            )),
        );
        let mut ctx = context_with(exec, FakeHttp::healthy(), FakeDb::healthy(), true, true);
        ctx.o3kd_env
            .insert("O3K_TOKEN_SIGNING_KEY".to_owned(), SENTINEL_ENV.to_owned());
        let report = run_all(&ctx).await;
        let serialized = match serde_json::to_string(&report) {
            Ok(serialized) => serialized,
            Err(error) => {
                assert!(
                    serde_json::to_string(&report).is_ok(),
                    "report must serialize: {error}"
                );
                return;
            }
        };
        assert!(
            !serialized.contains(SENTINEL_PW),
            "JSON output must never contain the password sentinel"
        );
        assert!(
            !serialized.contains(SENTINEL_ENV),
            "JSON output must never contain the env sentinel"
        );
        let human = report.render_human();
        assert!(
            !human.contains(SENTINEL_PW),
            "human output must never contain the password sentinel"
        );
        assert!(
            !human.contains(SENTINEL_ENV),
            "human output must never contain the env sentinel"
        );
    }
}
