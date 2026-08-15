//! Identity checks: the admin OpenRC credentials must be present, mode 0600,
//! complete, and accepted by the control-plane identity endpoint.

use crate::checks::internal_failure;
use crate::context::{Context, parse_env_contents};
use crate::output::{Category, Check, CheckStatus};

/// Keys that must carry a non-empty value in `admin-openrc`.
const REQUIRED_OPENRC_KEYS: [&str; 4] = [
    "OS_AUTH_URL",
    "OS_USERNAME",
    "OS_PASSWORD",
    "OS_PROJECT_NAME",
];

/// Reads and parses the admin OpenRC file through the exec seam.
async fn load_openrc(ctx: &Context) -> Result<std::collections::BTreeMap<String, String>, String> {
    let contents = ctx
        .exec
        .read_file(&ctx.admin_openrc_path())
        .map_err(|error| format!("admin-openrc is unreadable: {error}"))?;
    Ok(parse_env_contents(&contents))
}

/// `identity.configured`: `admin-openrc` must be a 0600 regular file with
/// all required identity parameters and API version 3.
pub async fn check_configured(ctx: &Context) -> Check {
    let path = ctx.admin_openrc_path();
    let status_path = format!("stat -c '%a %n' {}", path.display());
    let mut missing = Vec::new();
    if !ctx.exec.is_regular_file(&path) {
        return Check::new(
            "identity.configured",
            Category::Identity,
            CheckStatus::Fail,
            "admin-openrc is missing or not a regular file",
        )
        .with_actions(vec![
            format!("ls -l {}", path.display()),
            "re-create client credentials via the installer".to_owned(),
        ]);
    }
    match ctx.exec.file_mode(&path) {
        Ok(mode) if mode & 0o077 == 0 => {}
        Ok(mode) => {
            return Check::new(
                "identity.configured",
                Category::Identity,
                CheckStatus::Fail,
                format!(
                    "admin-openrc permissions are too open: {:04o}",
                    mode & 0o777
                ),
            )
            .with_actions(vec![status_path]);
        }
        Err(error) => {
            return internal_failure(
                "identity.configured",
                Category::Identity,
                "admin-openrc permissions",
                &error,
                vec![status_path],
            );
        }
    }
    let values = match load_openrc(ctx).await {
        Ok(values) => values,
        Err(error) => {
            return internal_failure(
                "identity.configured",
                Category::Identity,
                "admin-openrc",
                &error,
                vec![format!("ls -l {}", path.display())],
            );
        }
    };
    for key in REQUIRED_OPENRC_KEYS {
        if values.get(key).map(String::as_str).unwrap_or("").is_empty() {
            missing.push(key.to_owned());
        }
    }
    if values.get("OS_IDENTITY_API_VERSION").map(String::as_str) != Some("3") {
        missing.push("OS_IDENTITY_API_VERSION=3".to_owned());
    }
    if !missing.is_empty() {
        return Check::new(
            "identity.configured",
            Category::Identity,
            CheckStatus::Fail,
            format!("admin-openrc is missing {}", missing.join(", ")),
        )
        .with_actions(vec![
            format!("ls -l {}", path.display()),
            "re-create client credentials via the installer".to_owned(),
        ]);
    }
    Check::new(
        "identity.configured",
        Category::Identity,
        CheckStatus::Pass,
        "admin-openrc contains the required identity parameters",
    )
}

/// The Keystone v3 password-auth request body, built from the admin OpenRC
/// values (never printed).
fn token_request_body(values: &std::collections::BTreeMap<String, String>) -> String {
    let user = values.get("OS_USERNAME").map(String::as_str).unwrap_or("");
    let password = values.get("OS_PASSWORD").map(String::as_str).unwrap_or("");
    let project = values
        .get("OS_PROJECT_NAME")
        .map(String::as_str)
        .unwrap_or("");
    let user_domain = values
        .get("OS_USER_DOMAIN_NAME")
        .map(String::as_str)
        .unwrap_or("Default");
    let project_domain = values
        .get("OS_PROJECT_DOMAIN_NAME")
        .map(String::as_str)
        .unwrap_or("Default");
    serde_json::json!({
        "auth": {
            "identity": {
                "methods": ["password"],
                "password": {
                    "user": {
                        "name": user,
                        "domain": { "name": user_domain },
                        "password": password
                    }
                }
            },
            "scope": {
                "project": {
                    "name": project,
                    "domain": { "name": project_domain }
                }
            }
        }
    })
    .to_string()
}

/// `identity.authenticated`: POST the admin credentials to
/// `{OS_AUTH_URL}/auth/tokens`; 201 plus an `x-subject-token` header proves
/// the identity works. The summary never contains the password or token.
pub async fn check_authenticated(ctx: &Context) -> Check {
    let path = ctx.admin_openrc_path();
    if !ctx.exec.is_regular_file(&path) {
        return Check::new(
            "identity.authenticated",
            Category::Identity,
            CheckStatus::NotApplicable,
            "identity is not configured (see identity.configured)",
        );
    }
    let values = match load_openrc(ctx).await {
        Ok(values) => values,
        Err(error) => {
            return internal_failure(
                "identity.authenticated",
                Category::Identity,
                "admin-openrc",
                &error,
                vec![format!("ls -l {}", path.display())],
            );
        }
    };
    let auth_url = values.get("OS_AUTH_URL").map(String::as_str).unwrap_or("");
    if auth_url.is_empty() {
        return Check::new(
            "identity.authenticated",
            Category::Identity,
            CheckStatus::NotApplicable,
            "OS_AUTH_URL is not configured (see identity.configured)",
        );
    }
    let token_url = format!(
        "{}/auth/tokens",
        auth_url.strip_suffix('/').unwrap_or(auth_url)
    );
    let body = token_request_body(&values);
    let response = match ctx.http.post_json(&token_url, &body).await {
        Ok(response) => response,
        Err(error) => {
            return Check::new(
                "identity.authenticated",
                Category::Identity,
                CheckStatus::Fail,
                format!("identity endpoint unreachable: {error}"),
            )
            .with_actions(vec![
                "journalctl -u o3kd -n 100".to_owned(),
                "systemctl status o3kd".to_owned(),
            ]);
        }
    };
    if response.status == 201 && response.header("x-subject-token").is_some() {
        return Check::new(
            "identity.authenticated",
            Category::Identity,
            CheckStatus::Pass,
            "admin token issued successfully",
        );
    }
    if response.status == 401 {
        return Check::new(
            "identity.authenticated",
            Category::Identity,
            CheckStatus::Fail,
            "identity rejects admin credentials",
        )
        .with_actions(vec![
            "journalctl -u o3kd -n 100".to_owned(),
            "verify the seeded admin credentials against the installer".to_owned(),
        ]);
    }
    Check::new(
        "identity.authenticated",
        Category::Identity,
        CheckStatus::Warn,
        format!("unexpected identity response: HTTP {}", response.status),
    )
    .with_actions(vec![
        "journalctl -u o3kd -n 100".to_owned(),
        "systemctl status o3kd".to_owned(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::HttpResponse;
    use crate::testutil::{FakeDb, FakeExec, FakeHttp, context_with};

    fn context(exec: FakeExec, http: FakeHttp) -> Context {
        context_with(exec, http, FakeDb::healthy(), true, true)
    }

    #[tokio::test]
    async fn configured_passes_on_complete_openrc() {
        let check = check_configured(&context(FakeExec::healthy(), FakeHttp::healthy())).await;
        assert_eq!(check.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn configured_fails_when_openrc_missing() {
        let mut exec = FakeExec::healthy();
        exec.regular_files
            .retain(|path| path != "/etc/o3k/admin-openrc");
        let check = check_configured(&context(exec, FakeHttp::healthy())).await;
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn configured_fails_when_password_missing() {
        let mut exec = FakeExec::healthy();
        exec.files.insert(
            "/etc/o3k/admin-openrc".to_owned(),
            Ok("export OS_AUTH_URL=http://127.0.0.1:8080/v3\n\
                 export OS_USERNAME=admin\n\
                 export OS_PROJECT_NAME=admin\n\
                 export OS_IDENTITY_API_VERSION=3\n"
                .to_owned()),
        );
        let check = check_configured(&context(exec, FakeHttp::healthy())).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("OS_PASSWORD"));
        assert!(!check.summary.contains("fake-password"));
    }

    #[tokio::test]
    async fn configured_fails_when_mode_too_open() {
        let mut exec = FakeExec::healthy();
        exec.modes.insert("/etc/o3k/admin-openrc".to_owned(), 0o644);
        let check = check_configured(&context(exec, FakeHttp::healthy())).await;
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn authenticated_passes_on_201_with_token() {
        let check = check_authenticated(&context(FakeExec::healthy(), FakeHttp::healthy())).await;
        assert_eq!(check.status, CheckStatus::Pass);
        assert!(check.summary.contains("token issued"));
    }

    #[tokio::test]
    async fn authenticated_fails_on_401() {
        let mut http = FakeHttp::healthy();
        http.with(
            "POST http://127.0.0.1:8080/v3/auth/tokens",
            Ok(HttpResponse {
                status: 401,
                headers: Vec::new(),
                body: "{\"error\":{}}".to_owned(),
            }),
        );
        let check = check_authenticated(&context(FakeExec::healthy(), http)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("rejects"));
    }

    #[tokio::test]
    async fn authenticated_fails_when_unreachable() {
        let mut http = FakeHttp::healthy();
        http.with(
            "POST http://127.0.0.1:8080/v3/auth/tokens",
            Err("connection refused".to_owned()),
        );
        let check = check_authenticated(&context(FakeExec::healthy(), http)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("unreachable"));
    }
}
