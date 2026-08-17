//! Systemd service checks for the control-plane daemon and (libvirt profile
//! only) the compute agent.

use crate::checks::{compute_actions, not_libvirt_profile, o3kd_actions, profile_not_applicable};
use crate::context::{Context, UnitState};
use crate::output::{Category, Check, CheckStatus};

/// Evaluates one systemd unit probe into a check.
fn unit_check(id: &str, unit: &str, daemon: &str, state: UnitState, actions: Vec<String>) -> Check {
    match state {
        UnitState::Active => Check::new(
            id,
            Category::Services,
            CheckStatus::Pass,
            format!("{unit} is active"),
        ),
        UnitState::Inactive => Check::new(
            id,
            Category::Services,
            CheckStatus::Fail,
            format!("{unit} is inactive"),
        )
        .with_actions(actions),
        UnitState::Failed => Check::new(
            id,
            Category::Services,
            CheckStatus::Fail,
            format!("{unit} has failed"),
        )
        .with_actions(actions),
        UnitState::NotFound => Check::new(
            id,
            Category::Services,
            CheckStatus::Fail,
            format!("{daemon} systemd unit not found"),
        )
        .with_actions(actions),
        UnitState::Unknown => Check::new(
            id,
            Category::Services,
            CheckStatus::Warn,
            format!("{unit} state is unknown"),
        )
        .with_actions(actions),
    }
}

/// `services.o3kd_unit`: the control-plane daemon unit must be active.
pub async fn check_o3kd_unit(ctx: &Context) -> Check {
    if ctx.is_kubernetes() {
        return Check::new(
            "services.o3kd_unit",
            Category::Services,
            CheckStatus::NotApplicable,
            "systemd service unit checks are not applicable in Kubernetes deployment mode",
        );
    }
    let state = ctx.exec.systemctl_is_active("o3kd.service").await;
    unit_check(
        "services.o3kd_unit",
        "o3kd.service",
        "o3kd",
        state,
        o3kd_actions(),
    )
}

/// `services.compute_unit`: the compute agent unit must be active in the
/// libvirt profile.
pub async fn check_compute_unit(ctx: &Context) -> Check {
    if ctx.is_kubernetes() {
        return Check::new(
            "services.compute_unit",
            Category::Services,
            CheckStatus::NotApplicable,
            "systemd service unit checks are not applicable in Kubernetes deployment mode; compute agent runs externally",
        );
    }
    if not_libvirt_profile(ctx) {
        return profile_not_applicable("services.compute_unit", Category::Services);
    }
    let state = ctx.exec.systemctl_is_active("o3k-compute.service").await;
    unit_check(
        "services.compute_unit",
        "o3k-compute.service",
        "o3k-compute",
        state,
        compute_actions(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FakeDb, FakeExec, FakeHttp, context_with};

    fn context(exec: FakeExec) -> Context {
        context_with(exec, FakeHttp::healthy(), FakeDb::healthy(), true, true)
    }

    #[tokio::test]
    async fn o3kd_unit_passes_when_active() {
        let check = check_o3kd_unit(&context(FakeExec::healthy())).await;
        assert_eq!(check.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn o3kd_unit_fails_when_stopped() {
        let mut exec = FakeExec::healthy();
        exec.units
            .insert("o3kd.service".to_owned(), UnitState::Inactive);
        let check = check_o3kd_unit(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn o3kd_unit_fails_when_unit_not_found() {
        let mut exec = FakeExec::healthy();
        exec.units
            .insert("o3kd.service".to_owned(), UnitState::NotFound);
        let check = check_o3kd_unit(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("systemd unit not found"));
    }

    #[tokio::test]
    async fn compute_unit_fails_when_stopped() {
        let mut exec = FakeExec::healthy();
        exec.units
            .insert("o3k-compute.service".to_owned(), UnitState::Inactive);
        let check = check_compute_unit(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn compute_unit_not_applicable_without_profile() {
        let ctx = context_with(
            FakeExec::healthy(),
            FakeHttp::healthy(),
            FakeDb::healthy(),
            false,
            true,
        );
        let check = check_compute_unit(&ctx).await;
        assert_eq!(check.status, CheckStatus::NotApplicable);
    }
}
