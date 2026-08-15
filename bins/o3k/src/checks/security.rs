//! Security boundary checks: configuration file permissions and the libvirt
//! mTLS identity.

use crate::checks::{not_libvirt_profile, profile_not_applicable};
use crate::context::Context;
use crate::output::{Category, Check, CheckStatus};
use std::path::PathBuf;

/// TLS files that must all exist for a complete mTLS identity.
const TLS_IDENTITY_FILES: [&str; 7] = [
    "ca.pem",
    "server.pem",
    "server-key.pem",
    "agent.pem",
    "agent-key.pem",
    "agent-id",
    "agent-fingerprint",
];

/// `security.config_permissions`: env and credential files must be 0600,
/// private keys at most 0640, and the config directory must not be
/// world-writable. Only paths are reported, never contents.
pub async fn check_config_permissions(ctx: &Context) -> Check {
    let mut missing = Vec::new();
    let mut violations = Vec::new();

    // Files that must be exactly 0600.
    let mut restricted = vec![ctx.config_dir.join("o3kd.env")];
    if ctx.libvirt_profile {
        restricted.push(ctx.config_dir.join("o3k-compute.env"));
    }
    restricted.push(ctx.config_dir.join("admin-openrc"));
    restricted.push(ctx.config_dir.join("clouds.yaml"));
    for path in restricted {
        if !ctx.exec.is_regular_file(&path) {
            missing.push(path);
            continue;
        }
        match ctx.exec.file_mode(&path) {
            Ok(mode) if mode & 0o777 != 0o600 => {
                violations.push(format!("{:04o} {}", mode & 0o777, path.display()))
            }
            Ok(_) => {}
            Err(error) => violations.push(format!("{} ({error})", path.display())),
        }
    }

    // Private keys must be at most 0640 (no group/other write, no other
    // read).
    if ctx.libvirt_profile {
        for name in ["server-key.pem", "agent-key.pem"] {
            let path = ctx.tls_dir.join(name);
            if !ctx.exec.is_regular_file(&path) {
                missing.push(path);
                continue;
            }
            match ctx.exec.file_mode(&path) {
                Ok(mode) if mode & 0o777 & !0o640u32 != 0 => {
                    violations.push(format!("{:04o} {}", mode & 0o777, path.display()))
                }
                Ok(_) => {}
                Err(error) => violations.push(format!("{} ({error})", path.display())),
            }
        }
    }

    match ctx.exec.file_mode(&ctx.config_dir) {
        Ok(mode) if mode & 0o002 != 0 => violations.push(format!(
            "world-writable config directory: {:04o} {}",
            mode & 0o777,
            ctx.config_dir.display()
        )),
        Ok(_) => {}
        Err(error) => violations.push(format!("{} ({error})", ctx.config_dir.display())),
    }

    if !missing.is_empty() {
        return Check::new(
            "security.config_permissions",
            Category::Security,
            CheckStatus::Fail,
            "required configuration file missing",
        )
        .with_details(
            missing
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .with_actions(vec![format!("ls -l {}", ctx.config_dir.display())]);
    }
    if !violations.is_empty() {
        return Check::new(
            "security.config_permissions",
            Category::Security,
            CheckStatus::Fail,
            "configuration permissions are too open",
        )
        .with_details(violations.join("\n"))
        .with_actions(vec![format!(
            "stat -c '%a %n' {}/* {}/*",
            ctx.config_dir.display(),
            ctx.tls_dir.display()
        )]);
    }
    Check::new(
        "security.config_permissions",
        Category::Security,
        CheckStatus::Pass,
        "configuration file permissions are correct",
    )
}

/// `security.tls_identity`: all seven mTLS files must exist and the agent
/// fingerprint must match the authorized list in `o3kd.env`.
pub async fn check_tls_identity(ctx: &Context) -> Check {
    if not_libvirt_profile(ctx) {
        return profile_not_applicable("security.tls_identity", Category::Security);
    }
    let mut missing = Vec::new();
    for name in TLS_IDENTITY_FILES {
        let path: PathBuf = ctx.tls_dir.join(name);
        if !ctx.exec.is_regular_file(&path) {
            missing.push(name.to_owned());
        }
    }
    if !missing.is_empty() {
        return Check::new(
            "security.tls_identity",
            Category::Security,
            CheckStatus::Fail,
            "mTLS identity incomplete",
        )
        .with_details(format!("missing: {}", missing.join(", ")))
        .with_actions(vec![format!("ls -l {}", ctx.tls_dir.display())]);
    }
    let fingerprint = match ctx.exec.read_file(&ctx.tls_dir.join("agent-fingerprint")) {
        Ok(contents) => contents.trim().to_owned(),
        Err(error) => {
            return Check::new(
                "security.tls_identity",
                Category::Security,
                CheckStatus::Fail,
                "agent fingerprint is unreadable",
            )
            .with_details(error)
            .with_actions(vec![format!("ls -l {}", ctx.tls_dir.display())]);
        }
    };
    let authorized = ctx.o3kd_env.get("O3K_COMPUTE_AUTHORIZED_AGENTS");
    let Some(authorized) = authorized else {
        return Check::new(
            "security.tls_identity",
            Category::Security,
            CheckStatus::Fail,
            "authorized agent list is not configured in o3kd.env",
        )
        .with_actions(vec!["systemctl status o3kd".to_owned()]);
    };
    let (expected_agent_id, expected) = match authorized.split_once('=') {
        Some((agent_id, fingerprint)) if !agent_id.is_empty() => (agent_id, fingerprint),
        _ => ("", ""),
    };
    if expected.is_empty() || !fingerprint.eq_ignore_ascii_case(expected) {
        return Check::new(
            "security.tls_identity",
            Category::Security,
            CheckStatus::Fail,
            "agent fingerprint does not match authorized list: broken mTLS",
        )
        .with_actions(vec![
            format!("ls -l {}", ctx.tls_dir.display()),
            "systemctl status o3k-compute".to_owned(),
        ]);
    }
    // The authorized pair's agent id must match the on-disk identity too;
    // a fingerprint match against a mismatched id is still a broken pair.
    let agent_id = match ctx.exec.read_file(&ctx.tls_dir.join("agent-id")) {
        Ok(contents) => contents.trim().to_owned(),
        Err(error) => {
            return Check::new(
                "security.tls_identity",
                Category::Security,
                CheckStatus::Fail,
                "agent identity is unreadable",
            )
            .with_details(error)
            .with_actions(vec![format!("ls -l {}", ctx.tls_dir.display())]);
        }
    };
    if agent_id != expected_agent_id {
        return Check::new(
            "security.tls_identity",
            Category::Security,
            CheckStatus::Fail,
            "agent identity does not match the authorized list: broken mTLS",
        )
        .with_actions(vec![
            format!("ls -l {}", ctx.tls_dir.display()),
            "systemctl status o3k-compute".to_owned(),
        ]);
    }
    Check::new(
        "security.tls_identity",
        Category::Security,
        CheckStatus::Pass,
        "mTLS identity is complete and authorized",
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
    async fn tls_identity_passes_when_complete() {
        let check = check_tls_identity(&context(FakeExec::healthy())).await;
        assert_eq!(check.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn tls_identity_fails_on_fingerprint_mismatch() {
        let mut exec = FakeExec::healthy();
        exec.files.insert(
            "/etc/o3k/tls/agent-fingerprint".to_owned(),
            Ok("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_owned()),
        );
        let check = check_tls_identity(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("broken mTLS"));
    }

    #[tokio::test]
    async fn tls_identity_fails_when_file_missing() {
        let mut exec = FakeExec::healthy();
        exec.regular_files
            .retain(|path| path != "/etc/o3k/tls/agent-key.pem");
        let check = check_tls_identity(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("incomplete"));
    }

    #[tokio::test]
    async fn config_permissions_fails_on_world_writable_config_dir() {
        let mut exec = FakeExec::healthy();
        exec.modes.insert("/etc/o3k".to_owned(), 0o777);
        let check = check_config_permissions(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn config_permissions_fails_on_open_env_file() {
        let mut exec = FakeExec::healthy();
        exec.modes.insert("/etc/o3k/o3kd.env".to_owned(), 0o644);
        let check = check_config_permissions(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn tls_identity_not_applicable_without_profile() {
        let ctx = context_with(
            FakeExec::healthy(),
            FakeHttp::healthy(),
            FakeDb::healthy(),
            false,
            true,
        );
        let check = check_tls_identity(&ctx).await;
        assert_eq!(check.status, CheckStatus::NotApplicable);
    }

    #[tokio::test]
    async fn tls_identity_fails_on_agent_id_mismatch() {
        let mut exec = FakeExec::healthy();
        exec.files.insert(
            "/etc/o3k/tls/agent-id".to_owned(),
            Ok("wrong-agent".to_owned()),
        );
        let check = check_tls_identity(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("broken mTLS"));
    }
}
