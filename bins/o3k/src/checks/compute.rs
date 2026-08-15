//! Compute-agent checks against placement state, the control-plane
//! readiness probe, and the agent's self-reported epoch.

use crate::checks::control::readyz_response;
use crate::checks::{
    compute_actions, internal_failure, not_libvirt_profile, profile_not_applicable,
};
use crate::context::Context;
use crate::output::{Category, Check, CheckStatus};
use std::collections::BTreeMap;

/// Resource classes every compute provider must publish with a positive
/// total (mirrors `crates/o3k-placement`).
const REQUIRED_RESOURCE_CLASSES: [&str; 3] = ["VCPU", "MEMORY_MB", "DISK_GB"];

/// `compute.agent_registered`: at least one placement provider must exist
/// and the control plane must report ready. A registered provider with a
/// not-ready control plane means the agent is disconnected.
pub async fn check_agent_registered(ctx: &Context) -> Check {
    if not_libvirt_profile(ctx) {
        return profile_not_applicable("compute.agent_registered", Category::Compute);
    }
    let providers = match ctx.db.placement_providers(&ctx.database_path()).await {
        Ok(providers) => providers,
        Err(error) => {
            return internal_failure(
                "compute.agent_registered",
                Category::Compute,
                "compute agent registration",
                &error,
                compute_actions(),
            );
        }
    };
    if providers.is_empty() {
        return Check::new(
            "compute.agent_registered",
            Category::Compute,
            CheckStatus::Fail,
            "compute agent has never registered",
        )
        .with_actions(compute_actions());
    }
    let readyz = readyz_response(ctx).await;
    let ready = matches!(
        readyz,
        Ok(ref response) if response.status == 200 && response.body.contains("ready")
    );
    if ready {
        return Check::new(
            "compute.agent_registered",
            Category::Compute,
            CheckStatus::Pass,
            format!(
                "{} compute provider(s) registered and the control plane is ready",
                providers.len()
            ),
        );
    }
    Check::new(
        "compute.agent_registered",
        Category::Compute,
        CheckStatus::Fail,
        "agent disconnected: provider registered but control plane reports not ready",
    )
    .with_actions(compute_actions())
}

/// `compute.agent_epoch`: the agent's self-reported epoch must not be older
/// than any epoch the control plane has durably persisted. A newer
/// self-reported epoch is a healthy registration that has not been durably
/// recorded yet (WARN); an older one is a stale epoch (FAIL). UUIDv7 epoch
/// strings compare lexicographically in time order.
pub async fn check_agent_epoch(ctx: &Context) -> Check {
    if not_libvirt_profile(ctx) {
        return profile_not_applicable("compute.agent_epoch", Category::Compute);
    }
    let url = format!("http://{}/readyz", ctx.compute_health_addr);
    let self_reported = match ctx.http.get(&url).await {
        Ok(response) => match serde_json::from_str::<serde_json::Value>(&response.body) {
            Ok(value) => value
                .get("agent_epoch")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            Err(_) => None,
        },
        Err(_) => None,
    };
    let Some(self_reported) = self_reported else {
        return Check::new(
            "compute.agent_epoch",
            Category::Compute,
            CheckStatus::Warn,
            "agent does not self-report its epoch",
        )
        .with_actions(vec![
            format!("curl -s {url}"),
            "journalctl -u o3k-compute -n 100".to_owned(),
        ]);
    };
    let epochs = match ctx.db.latest_epochs(&ctx.database_path()).await {
        Ok(epochs) => epochs,
        Err(error) => {
            return internal_failure(
                "compute.agent_epoch",
                Category::Compute,
                "persisted agent epochs",
                &error,
                compute_actions(),
            );
        }
    };
    if epochs.is_empty() {
        return Check::new(
            "compute.agent_epoch",
            Category::Compute,
            CheckStatus::NotApplicable,
            "no persisted agent epoch yet",
        );
    }
    let mut stale: Vec<String> = Vec::new();
    let mut not_recorded: Vec<String> = Vec::new();
    for epoch in &epochs {
        let record = format!(
            "{} agent {} persisted {}",
            epoch.source, epoch.agent_id, epoch.agent_epoch
        );
        // UUIDv7 strings order lexicographically by creation time: a
        // persisted epoch NEWER than the agent's self-report means the agent
        // is behind (stale); an older persisted epoch is just a registration
        // that has no durable trace yet.
        if epoch.agent_epoch > self_reported {
            stale.push(record);
        } else if epoch.agent_epoch < self_reported {
            not_recorded.push(record);
        }
    }
    if stale.is_empty() && not_recorded.is_empty() {
        return Check::new(
            "compute.agent_epoch",
            Category::Compute,
            CheckStatus::Pass,
            "agent epoch matches every persisted control-plane epoch",
        );
    }
    if !stale.is_empty() {
        return Check::new(
            "compute.agent_epoch",
            Category::Compute,
            CheckStatus::Fail,
            format!(
                "stale epoch: agent reports {self_reported}, control plane persisted newer epochs ({})",
                stale.join(", ")
            ),
        )
        .with_actions(vec![
            "journalctl -u o3kd -n 100".to_owned(),
            "journalctl -u o3k-compute -n 100".to_owned(),
        ]);
    }
    Check::new(
        "compute.agent_epoch",
        Category::Compute,
        CheckStatus::Warn,
        format!(
            "agent epoch {self_reported} is newer than the persisted control-plane epochs ({})",
            not_recorded.join(", ")
        ),
    )
    .with_actions(vec![
        "journalctl -u o3kd -n 100".to_owned(),
        "journalctl -u o3k-compute -n 100".to_owned(),
    ])
}

/// `compute.agent_capabilities`: every provider must publish a positive
/// total for VCPU, MEMORY_MB, and DISK_GB.
pub async fn check_agent_capabilities(ctx: &Context) -> Check {
    if not_libvirt_profile(ctx) {
        return profile_not_applicable("compute.agent_capabilities", Category::Compute);
    }
    let providers = match ctx.db.placement_providers(&ctx.database_path()).await {
        Ok(providers) => providers,
        Err(error) => {
            return internal_failure(
                "compute.agent_capabilities",
                Category::Compute,
                "placement providers",
                &error,
                compute_actions(),
            );
        }
    };
    if providers.is_empty() {
        return Check::new(
            "compute.agent_capabilities",
            Category::Compute,
            CheckStatus::NotApplicable,
            "no compute providers registered",
        );
    }
    let inventories = match ctx.db.placement_inventories(&ctx.database_path()).await {
        Ok(inventories) => inventories,
        Err(error) => {
            return internal_failure(
                "compute.agent_capabilities",
                Category::Compute,
                "placement inventories",
                &error,
                compute_actions(),
            );
        }
    };
    let mut by_provider: BTreeMap<&str, BTreeMap<&str, i64>> = BTreeMap::new();
    for inventory in &inventories {
        by_provider
            .entry(inventory.provider_id.as_str())
            .or_default()
            .insert(inventory.resource_class.as_str(), inventory.total);
    }
    let mut findings = Vec::new();
    for provider in &providers {
        let totals = by_provider.get(provider.id.as_str());
        for class in REQUIRED_RESOURCE_CLASSES {
            let total = totals.and_then(|map| map.get(class)).copied().unwrap_or(0);
            if total <= 0 {
                findings.push(format!(
                    "provider {}: {class} total is {total}",
                    provider.id
                ));
            }
        }
    }
    if findings.is_empty() {
        return Check::new(
            "compute.agent_capabilities",
            Category::Compute,
            CheckStatus::Pass,
            "every provider publishes positive VCPU, MEMORY_MB, and DISK_GB totals",
        );
    }
    Check::new(
        "compute.agent_capabilities",
        Category::Compute,
        CheckStatus::Fail,
        "compute provider capabilities are incomplete",
    )
    .with_details(findings.join("\n"))
    .with_actions(compute_actions())
}

/// `compute.placement_consistent`: the stored `used` column of each
/// inventory row must equal the sum of live allocations for that
/// (provider, resource class) pair.
pub async fn check_placement_consistent(ctx: &Context) -> Check {
    let providers = match ctx.db.placement_providers(&ctx.database_path()).await {
        Ok(providers) => providers,
        Err(error) => {
            return internal_failure(
                "compute.placement_consistent",
                Category::Compute,
                "placement providers",
                &error,
                compute_actions(),
            );
        }
    };
    if providers.is_empty() {
        return Check::new(
            "compute.placement_consistent",
            Category::Compute,
            CheckStatus::NotApplicable,
            "no compute providers registered",
        );
    }
    let inventories = match ctx.db.placement_inventories(&ctx.database_path()).await {
        Ok(inventories) => inventories,
        Err(error) => {
            return internal_failure(
                "compute.placement_consistent",
                Category::Compute,
                "placement inventories",
                &error,
                compute_actions(),
            );
        }
    };
    let allocations = match ctx.db.live_allocation_resources(&ctx.database_path()).await {
        Ok(allocations) => allocations,
        Err(error) => {
            return internal_failure(
                "compute.placement_consistent",
                Category::Compute,
                "live allocation sums",
                &error,
                compute_actions(),
            );
        }
    };
    let mut computed: BTreeMap<(&str, &str), i64> = BTreeMap::new();
    for allocation in &allocations {
        *computed
            .entry((
                allocation.provider_id.as_str(),
                allocation.resource_class.as_str(),
            ))
            .or_insert(0) += allocation.amount;
    }
    let mut findings = Vec::new();
    for inventory in &inventories {
        let computed_used = computed
            .get(&(
                inventory.provider_id.as_str(),
                inventory.resource_class.as_str(),
            ))
            .copied()
            .unwrap_or(0);
        if computed_used != inventory.used {
            findings.push(format!(
                "provider {} class {}: stored used {}, computed {}",
                inventory.provider_id, inventory.resource_class, inventory.used, computed_used
            ));
        }
    }
    if findings.is_empty() {
        return Check::new(
            "compute.placement_consistent",
            Category::Compute,
            CheckStatus::Pass,
            "stored placement usage matches live allocations",
        );
    }
    Check::new(
        "compute.placement_consistent",
        Category::Compute,
        CheckStatus::Fail,
        "placement usage is inconsistent with live allocations",
    )
    .with_details(findings.join("\n"))
    .with_actions(compute_actions())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::HttpResponse;
    use crate::testutil::{FakeDb, FakeExec, FakeHttp, context_with};

    fn context(exec: FakeExec, http: FakeHttp, db: FakeDb) -> Context {
        context_with(exec, http, db, true, true)
    }

    #[tokio::test]
    async fn agent_registered_fails_when_readyz_not_ready() {
        let mut http = FakeHttp::healthy();
        http.with(
            "GET http://127.0.0.1:8080/readyz",
            Ok(HttpResponse {
                status: 503,
                headers: Vec::new(),
                body: "{\"status\":\"not_ready\"}".to_owned(),
            }),
        );
        let check =
            check_agent_registered(&context(FakeExec::healthy(), http, FakeDb::healthy())).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("disconnected"));
    }

    #[tokio::test]
    async fn agent_registered_fails_when_never_registered() {
        let mut db = FakeDb::healthy();
        db.providers.clear();
        let check =
            check_agent_registered(&context(FakeExec::healthy(), FakeHttp::healthy(), db)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("never registered"));
    }

    #[tokio::test]
    async fn agent_registered_not_applicable_without_profile() {
        let ctx = context_with(
            FakeExec::healthy(),
            FakeHttp::healthy(),
            FakeDb::healthy(),
            false,
            true,
        );
        let check = check_agent_registered(&ctx).await;
        assert_eq!(check.status, CheckStatus::NotApplicable);
    }

    #[tokio::test]
    async fn agent_epoch_fails_when_stale() {
        // The control plane persisted a NEWER epoch than the agent reports:
        // the agent is behind (stale). UUIDv7 strings order by time.
        let mut db = FakeDb::healthy();
        db.epochs = vec![crate::db::EpochRow {
            source: "observation_watermarks".to_owned(),
            agent_id: String::new(),
            agent_epoch: "43".to_owned(),
        }];
        let check = check_agent_epoch(&context(FakeExec::healthy(), FakeHttp::healthy(), db)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("stale epoch"));
    }

    #[tokio::test]
    async fn agent_epoch_warns_when_registration_not_recorded() {
        // The agent reports a NEWER epoch than anything persisted: a healthy
        // re-registration without a durable trace yet — never a failure.
        let mut db = FakeDb::healthy();
        db.epochs = vec![crate::db::EpochRow {
            source: "agent_commands".to_owned(),
            agent_id: String::new(),
            agent_epoch: "41".to_owned(),
        }];
        let check = check_agent_epoch(&context(FakeExec::healthy(), FakeHttp::healthy(), db)).await;
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check.summary.contains("not been durably recorded") || check.summary.contains("newer")
        );
    }

    #[tokio::test]
    async fn agent_epoch_warns_without_self_report() {
        let mut http = FakeHttp::healthy();
        http.with(
            "GET http://127.0.0.1:9100/readyz",
            Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: "{\"status\":\"ready\"}".to_owned(),
            }),
        );
        let check = check_agent_epoch(&context(FakeExec::healthy(), http, FakeDb::healthy())).await;
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.summary.contains("self-report"));
    }

    #[tokio::test]
    async fn agent_capabilities_fails_when_class_missing() {
        let mut db = FakeDb::healthy();
        db.inventories
            .retain(|inventory| inventory.resource_class != "DISK_GB");
        let check =
            check_agent_capabilities(&context(FakeExec::healthy(), FakeHttp::healthy(), db)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(
            check
                .details
                .as_deref()
                .is_some_and(|d| d.contains("DISK_GB"))
        );
    }

    #[tokio::test]
    async fn placement_consistent_fails_on_mismatch() {
        let mut db = FakeDb::healthy();
        for inventory in &mut db.inventories {
            if inventory.resource_class == "VCPU" {
                inventory.used = 99;
            }
        }
        let check =
            check_placement_consistent(&context(FakeExec::healthy(), FakeHttp::healthy(), db))
                .await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(
            check
                .details
                .as_deref()
                .is_some_and(|d| d.contains("stored used 99"))
        );
    }

    #[tokio::test]
    async fn agent_epoch_not_applicable_without_profile() {
        let ctx = context_with(
            FakeExec::healthy(),
            FakeHttp::healthy(),
            FakeDb::healthy(),
            false,
            true,
        );
        let check = check_agent_epoch(&ctx).await;
        assert_eq!(check.status, CheckStatus::NotApplicable);
    }

    #[tokio::test]
    async fn agent_capabilities_not_applicable_without_profile() {
        let ctx = context_with(
            FakeExec::healthy(),
            FakeHttp::healthy(),
            FakeDb::healthy(),
            false,
            true,
        );
        let check = check_agent_capabilities(&ctx).await;
        assert_eq!(check.status, CheckStatus::NotApplicable);
    }
}
