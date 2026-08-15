//! Control-plane API checks against the loopback HTTP listener.

use crate::checks::o3kd_actions;
use crate::context::{Context, HttpResponse};
use crate::output::{Category, Check, CheckStatus};

/// `control.healthz`: the control plane liveness probe must answer 200 with
/// an "ok" body.
pub async fn check_healthz(ctx: &Context) -> Check {
    let url = format!("http://{}/healthz", ctx.listen_addr);
    let response = match ctx.http.get(&url).await {
        Ok(response) => response,
        Err(error) => {
            return Check::new(
                "control.healthz",
                Category::Control,
                CheckStatus::Fail,
                format!("control plane API unreachable: {error}"),
            )
            .with_actions(o3kd_actions());
        }
    };
    if response.status == 200 && response.body.contains("ok") {
        return Check::new(
            "control.healthz",
            Category::Control,
            CheckStatus::Pass,
            "control plane liveness probe answered",
        );
    }
    Check::new(
        "control.healthz",
        Category::Control,
        CheckStatus::Warn,
        format!("unexpected healthz response: HTTP {}", response.status),
    )
    .with_actions(vec![
        format!("curl -s {url}"),
        "systemctl status o3kd".to_owned(),
    ])
}

/// `control.readyz`: 200 with "ready" is healthy; 503 means the compute
/// agent is not ready (a FAIL for the libvirt profile).
pub async fn check_readyz(ctx: &Context) -> Check {
    let url = format!("http://{}/readyz", ctx.listen_addr);
    let response = match ctx.http.get(&url).await {
        Ok(response) => response,
        Err(error) => {
            return Check::new(
                "control.readyz",
                Category::Control,
                CheckStatus::Fail,
                format!("control plane API unreachable: {error}"),
            )
            .with_actions(o3kd_actions());
        }
    };
    if response.status == 200 && response.body.contains("ready") {
        return Check::new(
            "control.readyz",
            Category::Control,
            CheckStatus::Pass,
            "control plane reports ready",
        );
    }
    if response.status == 503 {
        let summary = if ctx.libvirt_profile {
            "control plane not ready: compute agent not ready"
        } else {
            "control plane not ready: provider capability probe failed"
        };
        return Check::new(
            "control.readyz",
            Category::Control,
            CheckStatus::Fail,
            summary,
        )
        .with_actions(o3kd_actions());
    }
    Check::new(
        "control.readyz",
        Category::Control,
        CheckStatus::Warn,
        format!("unexpected readyz response: HTTP {}", response.status),
    )
    .with_actions(vec![
        format!("curl -s {url}"),
        "systemctl status o3kd".to_owned(),
    ])
}

/// Re-runs the readiness probe for other checks that branch on its result
/// (the response is small and loopback-local).
pub(crate) async fn readyz_response(ctx: &Context) -> Result<HttpResponse, String> {
    let url = format!("http://{}/readyz", ctx.listen_addr);
    ctx.http.get(&url).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FakeDb, FakeExec, FakeHttp, context_with};

    fn context(http: FakeHttp) -> Context {
        context_with(FakeExec::healthy(), http, FakeDb::healthy(), true, true)
    }

    #[tokio::test]
    async fn healthz_passes_on_200_ok() {
        let check = check_healthz(&context(FakeHttp::healthy())).await;
        assert_eq!(check.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn healthz_fails_when_unreachable() {
        let mut http = FakeHttp::healthy();
        http.with(
            "GET http://127.0.0.1:8080/healthz",
            Err("connection refused".to_owned()),
        );
        let check = check_healthz(&context(http)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("unreachable"));
    }

    #[tokio::test]
    async fn readyz_passes_on_200_ready() {
        let check = check_readyz(&context(FakeHttp::healthy())).await;
        assert_eq!(check.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn readyz_fails_on_503() {
        let mut http = FakeHttp::healthy();
        http.with(
            "GET http://127.0.0.1:8080/readyz",
            Ok(HttpResponse {
                status: 503,
                headers: Vec::new(),
                body: "{\"status\":\"not_ready\"}".to_owned(),
            }),
        );
        let check = check_readyz(&context(http)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("not ready"));
    }
}
