//! The 34 doctor checks, grouped by output category. Each check is a
//! `pub async fn check(ctx: &Context) -> Check`; internal errors are always
//! converted to a sanitized FAIL, never a panic.

pub mod cloud;
pub mod compute;
pub mod control;
pub mod database;
pub mod host;
pub mod identity;
pub mod libvirt;
pub mod network;
pub mod release;
pub mod security;
pub mod services;

use crate::context::{Context, sanitize_error};
use crate::output::{Category, Check, CheckStatus};

/// Converts an internal check error into a FAIL with a sanitized summary.
#[must_use]
pub fn internal_failure(
    id: &str,
    category: Category,
    what: &str,
    error: &str,
    actions: Vec<String>,
) -> Check {
    Check::new(
        id,
        category,
        CheckStatus::Fail,
        format!(
            "internal error while checking {what}: {}",
            sanitize_error(error)
        ),
    )
    .with_actions(actions)
}

/// Standard read-only actions for the control-plane daemon.
#[must_use]
pub fn o3kd_actions() -> Vec<String> {
    vec![
        "systemctl status o3kd".to_owned(),
        "journalctl -u o3kd -n 100".to_owned(),
    ]
}

/// Standard read-only actions for the compute agent.
#[must_use]
pub fn compute_actions() -> Vec<String> {
    vec![
        "systemctl status o3k-compute".to_owned(),
        "journalctl -u o3k-compute -n 100".to_owned(),
    ]
}

/// The stable libvirt domain name of an O3K server, mirroring
/// `o3k-libvirt::stable_domain_name`: `o3k-` plus the first 20 lowercase hex
/// characters of the SHA-256 of the server id string.
#[must_use]
pub fn stable_domain_name(server_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(server_id.as_bytes());
    let mut hex = String::with_capacity(40);
    for byte in digest.iter().take(10) {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    format!("o3k-{hex}")
}

/// Whether a libvirt domain name follows the O3K managed naming pattern.
#[must_use]
pub fn is_o3k_domain_name(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix("o3k-") else {
        return false;
    };
    suffix.len() == 20 && suffix.bytes().all(|b| b.is_ascii_hexdigit())
}

/// True when a context belongs to the installed libvirt profile.
#[must_use]
pub fn not_libvirt_profile(ctx: &Context) -> bool {
    !ctx.libvirt_profile
}

/// Builds the standard NOT_APPLICABLE check for the compute-agent profile.
#[must_use]
pub fn profile_not_applicable(id: &str, category: Category) -> Check {
    Check::new(
        id,
        category,
        CheckStatus::NotApplicable,
        "the compute agent is not part of this installation profile",
    )
}
