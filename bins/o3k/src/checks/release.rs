//! Installed release checks: the release manifest, the installer ownership
//! manifest, and the SHA-256 sums of the installed binaries.

use crate::checks::internal_failure;
use crate::context::Context;
use crate::output::{Category, Check, CheckStatus};

/// `release.version`: `<prefix>/share/o3k/release-manifest.json` must exist
/// and carry a non-empty `version` string.
pub async fn check_version(ctx: &Context) -> Check {
    for prefix in &ctx.prefix_candidates {
        let manifest = prefix.join("share/o3k/release-manifest.json");
        if !ctx.exec.is_regular_file(&manifest) {
            continue;
        }
        let contents = match ctx.exec.read_file(&manifest) {
            Ok(contents) => contents,
            Err(error) => {
                return internal_failure(
                    "release.version",
                    Category::Release,
                    "the release manifest",
                    &error,
                    vec![format!("ls -l {}", manifest.display())],
                );
            }
        };
        match serde_json::from_str::<serde_json::Value>(&contents) {
            Ok(value) => {
                let version = value
                    .get("version")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if !version.is_empty() {
                    return Check::new(
                        "release.version",
                        Category::Release,
                        CheckStatus::Pass,
                        format!("installed release manifest declares version {version}"),
                    );
                }
                return Check::new(
                    "release.version",
                    Category::Release,
                    CheckStatus::Fail,
                    "release manifest has no version",
                )
                .with_actions(vec![format!("cat {}", manifest.display())]);
            }
            Err(error) => {
                return Check::new(
                    "release.version",
                    Category::Release,
                    CheckStatus::Fail,
                    "release manifest is not valid JSON",
                )
                .with_details(crate::context::sanitize_error(&error.to_string()))
                .with_actions(vec![format!("ls -l {}", manifest.display())]);
            }
        }
    }
    Check::new(
        "release.version",
        Category::Release,
        CheckStatus::Fail,
        "installer release manifest missing",
    )
    .with_actions(vec![
        "re-run the one-line installer (packaging/get-o3k.sh)".to_owned(),
    ])
}

/// `release.ownership_manifest`: `<prefix>/share/o3k/.o3k-installed` must
/// carry the `o3k-installed-v1 prefix=<prefix>` header and every recorded
/// relative path must exist.
pub async fn check_ownership_manifest(ctx: &Context) -> Check {
    let mut checked_any = false;
    let mut findings = Vec::new();
    for prefix in &ctx.prefix_candidates {
        let manifest = prefix.join("share/o3k/.o3k-installed");
        if !ctx.exec.is_regular_file(&manifest) {
            continue;
        }
        checked_any = true;
        let contents = match ctx.exec.read_file(&manifest) {
            Ok(contents) => contents,
            Err(error) => {
                return internal_failure(
                    "release.ownership_manifest",
                    Category::Release,
                    "the installer ownership manifest",
                    &error,
                    vec![format!("ls -l {}", manifest.display())],
                );
            }
        };
        let mut lines = contents.lines();
        match lines.next() {
            Some(header) if header == format!("o3k-installed-v1 prefix={}", prefix.display()) => {}
            Some(header) => {
                findings.push(format!("unrecognized header: {header}"));
                continue;
            }
            None => {
                findings.push("ownership manifest is empty".to_owned());
                continue;
            }
        }
        for entry in lines {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let path = prefix.join(entry);
            if !ctx.exec.is_regular_file(&path) {
                findings.push(format!("missing installed file: {entry}"));
            }
        }
    }
    if !checked_any {
        return Check::new(
            "release.ownership_manifest",
            Category::Release,
            CheckStatus::Fail,
            "installer ownership manifest missing",
        )
        .with_actions(vec![
            "re-run the one-line installer (packaging/get-o3k.sh)".to_owned(),
        ]);
    }
    if findings.is_empty() {
        return Check::new(
            "release.ownership_manifest",
            Category::Release,
            CheckStatus::Pass,
            "installer ownership manifest is complete",
        );
    }
    Check::new(
        "release.ownership_manifest",
        Category::Release,
        CheckStatus::Fail,
        "installer ownership manifest violations",
    )
    .with_details(findings.join("\n"))
    .with_actions(vec![
        "re-run the one-line installer (packaging/get-o3k.sh)".to_owned(),
    ])
}

/// `release.binary_hashes`: each installed binary must match its reference
/// SHA-256 in `<prefix>/share/o3k/SHA256SUMS`. A missing reference is a
/// WARN; a mismatching digest is a FAIL.
pub async fn check_binary_hashes(ctx: &Context) -> Check {
    let binaries: Vec<&str> = if ctx.libvirt_profile {
        vec!["o3kd", "o3k", "o3k-compute"]
    } else {
        vec!["o3kd", "o3k"]
    };
    let mut modified = Vec::new();
    let mut missing_binaries = Vec::new();
    let mut warns = Vec::new();
    for name in binaries {
        let Some(prefix) = ctx
            .prefix_candidates
            .iter()
            .find(|prefix| ctx.exec.is_regular_file(&prefix.join("bin").join(name)))
        else {
            missing_binaries.push(name.to_owned());
            continue;
        };
        let binary = prefix.join("bin").join(name);
        let sums = prefix.join("share/o3k/SHA256SUMS");
        if !ctx.exec.is_regular_file(&sums) {
            warns.push(format!("no reference hash available for {name}"));
            continue;
        }
        let contents = match ctx.exec.read_file(&sums) {
            Ok(contents) => contents,
            Err(error) => {
                return internal_failure(
                    "release.binary_hashes",
                    Category::Release,
                    "the SHA256SUMS reference",
                    &error,
                    vec![format!("ls -l {}", sums.display())],
                );
            }
        };
        let mut expected = None;
        for line in contents.lines() {
            // Lines are `hash  ./o3k-<version>/bin/<name>`.
            let mut fields = line.split_whitespace();
            let Some(hash) = fields.next() else {
                continue;
            };
            let Some(path) = fields.next() else {
                continue;
            };
            if path.ends_with(&format!("/bin/{name}")) {
                expected = Some(hash.to_owned());
                break;
            }
        }
        let Some(expected) = expected else {
            warns.push(format!("no reference hash available for {name}"));
            continue;
        };
        let actual = match ctx.exec.sha256_file(&binary) {
            Ok(digest) => digest,
            Err(error) => {
                return internal_failure(
                    "release.binary_hashes",
                    Category::Release,
                    &format!("hashing {name}"),
                    &error,
                    vec![format!("ls -l {}", binary.display())],
                );
            }
        };
        if !actual.eq_ignore_ascii_case(&expected) {
            modified.push(name.to_owned());
        }
    }
    if modified.is_empty() && missing_binaries.is_empty() && warns.is_empty() {
        return Check::new(
            "release.binary_hashes",
            Category::Release,
            CheckStatus::Pass,
            "installed binaries match their reference hashes",
        );
    }
    let mut details = Vec::new();
    for name in &missing_binaries {
        details.push(format!("installed binary missing: {name}"));
    }
    for warning in &warns {
        details.push(warning.clone());
    }
    for name in &modified {
        details.push(format!("modified installed binary: {name}"));
    }
    let status = if modified.is_empty() && missing_binaries.is_empty() {
        CheckStatus::Warn
    } else {
        CheckStatus::Fail
    };
    let summary = if !modified.is_empty() {
        format!("modified installed binary: {}", modified.join(", "))
    } else if !missing_binaries.is_empty() {
        format!("installed binary missing: {}", missing_binaries.join(", "))
    } else {
        "some installed binaries lack a reference hash".to_owned()
    };
    let prefix_action = ctx
        .prefix_candidates
        .first()
        .map(|prefix| {
            format!(
                "sha256sum {}/bin/o3kd {}/bin/o3k",
                prefix.display(),
                prefix.display()
            )
        })
        .unwrap_or_else(|| "sha256sum /usr/local/bin/o3kd /usr/local/bin/o3k".to_owned());
    Check::new("release.binary_hashes", Category::Release, status, summary)
        .with_details(details.join("\n"))
        .with_actions(vec![
            prefix_action,
            "re-run the one-line installer (packaging/get-o3k.sh)".to_owned(),
        ])
}

/// `release.binary_set_consistent`: every installed binary hashes against
/// the installed SHA256SUMS and all reference entries declare the same
/// release version (a mixed-version install is a FAIL).
pub async fn check_binary_set_consistent(ctx: &Context) -> Check {
    let binaries: Vec<&str> = if ctx.libvirt_profile {
        vec!["o3kd", "o3k", "o3k-compute"]
    } else {
        vec!["o3kd", "o3k"]
    };
    let Some(prefix) = ctx.prefix_candidates.iter().find(|prefix| {
        binaries
            .iter()
            .all(|name| ctx.exec.is_regular_file(&prefix.join("bin").join(name)))
    }) else {
        return Check::new(
            "release.binary_set_consistent",
            Category::Release,
            CheckStatus::Fail,
            "installed binaries are missing",
        )
        .with_actions(vec![
            "re-run the one-line installer (packaging/get-o3k.sh)".to_owned(),
        ]);
    };
    let sums = prefix.join("share/o3k/SHA256SUMS");
    if !ctx.exec.is_regular_file(&sums) {
        return Check::new(
            "release.binary_set_consistent",
            Category::Release,
            CheckStatus::Warn,
            "no installed SHA256SUMS reference; the binary set cannot be verified",
        )
        .with_actions(vec![format!("ls -l {}", sums.display())]);
    }
    let contents = match ctx.exec.read_file(&sums) {
        Ok(contents) => contents,
        Err(error) => {
            return internal_failure(
                "release.binary_set_consistent",
                Category::Release,
                "the SHA256SUMS reference",
                &error,
                vec![format!("ls -l {}", sums.display())],
            );
        }
    };
    let mut references: std::collections::BTreeMap<&str, (String, String)> =
        std::collections::BTreeMap::new();
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let Some(hash) = fields.next() else {
            continue;
        };
        let Some(path) = fields.next() else {
            continue;
        };
        let Some(name) = path.rsplit('/').next() else {
            continue;
        };
        if !binaries.contains(&name) {
            continue;
        }
        // `./o3k-<version>/bin/<name>` -> version.
        let version = path
            .strip_prefix("./o3k-")
            .and_then(|rest| rest.strip_suffix(&format!("/bin/{name}")))
            .unwrap_or("unknown")
            .to_owned();
        references.insert(name, (hash.to_owned(), version));
    }
    let mut distinct_versions: Vec<&str> = references
        .values()
        .map(|(_, version)| version.as_str())
        .filter(|version| *version != "unknown")
        .collect();
    distinct_versions.sort_unstable();
    distinct_versions.dedup();
    if distinct_versions.len() > 1 {
        return Check::new(
            "release.binary_set_consistent",
            Category::Release,
            CheckStatus::Fail,
            "mixed release versions in the installed SHA256SUMS",
        )
        .with_details(format!("versions: {}", distinct_versions.join(", ")))
        .with_actions(vec![
            format!("cat {}", sums.display()),
            "re-run the one-line installer (packaging/get-o3k.sh)".to_owned(),
        ]);
    }
    let mut findings = Vec::new();
    for name in &binaries {
        let Some((expected, _)) = references.get(name) else {
            findings.push(format!("no reference hash for {name}"));
            continue;
        };
        let binary = prefix.join("bin").join(name);
        let actual = match ctx.exec.sha256_file(&binary) {
            Ok(digest) => digest,
            Err(error) => {
                return internal_failure(
                    "release.binary_set_consistent",
                    Category::Release,
                    &format!("hashing {name}"),
                    &error,
                    vec![format!("ls -l {}", binary.display())],
                );
            }
        };
        if !actual.eq_ignore_ascii_case(expected) {
            findings.push(format!("{name} does not match the installed SHA256SUMS"));
        }
    }
    if findings.is_empty() {
        return Check::new(
            "release.binary_set_consistent",
            Category::Release,
            CheckStatus::Pass,
            "installed binaries are a consistent, verified set",
        );
    }
    Check::new(
        "release.binary_set_consistent",
        Category::Release,
        CheckStatus::Fail,
        "installed binary set is inconsistent",
    )
    .with_details(findings.join("\n"))
    .with_actions(vec![
        "re-run the one-line installer (packaging/get-o3k.sh)".to_owned(),
    ])
}

/// `release.backup_available`: PASS when the rollback chain holds a valid
/// O3K-created backup record, WARN when absent on an otherwise healthy
/// install (doctor never repairs, never creates backups).
pub async fn check_backup_available(ctx: &Context) -> Check {
    let backup_dir = std::env::var("O3K_UPGRADE_BACKUP_DIR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| ctx.data_dir.join("backups"));
    let chain = backup_dir.join("backup-chain.json");
    let contents = match ctx.exec.read_file(&chain) {
        Ok(contents) => contents,
        Err(_) => {
            return Check::new(
                "release.backup_available",
                Category::Release,
                CheckStatus::Warn,
                "no O3K-created backup exists yet",
            )
            .with_actions(vec![
                "sudo o3k upgrade".to_owned(),
                "sudo o3k upgrade --check".to_owned(),
            ]);
        }
    };
    match serde_json::from_str::<crate::upgrade::RollbackChain>(&contents)
        .map_err(|error| error.to_string())
        .and_then(|chain| {
            chain.validate()?;
            Ok(chain)
        })
        .and_then(|chain| {
            if chain
                .backups
                .iter()
                .any(|record| record.kind == crate::upgrade::RecordKind::Backup)
            {
                Ok(())
            } else {
                Err("no eligible backup record".to_owned())
            }
        }) {
        Ok(()) => Check::new(
            "release.backup_available",
            Category::Release,
            CheckStatus::Pass,
            "a verified O3K-created backup exists in the rollback chain",
        ),
        Err(error) => Check::new(
            "release.backup_available",
            Category::Release,
            CheckStatus::Warn,
            format!(
                "no valid O3K-created backup record: {}",
                crate::context::sanitize_error(&error.to_string())
            ),
        )
        .with_actions(vec![
            "sudo o3k upgrade".to_owned(),
            "sudo o3k upgrade --check".to_owned(),
        ]),
    }
}

/// `release.upgrade_state`: PASS when no state file exists or the recorded
/// phase is COMMITTED; WARN (upgrade incomplete) with the exact safe next
/// command when an in-progress or FAILED_UPGRADE state exists.
pub async fn check_upgrade_state(ctx: &Context) -> Check {
    let state_file = std::env::var("O3K_UPGRADE_STATE_FILE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| ctx.data_dir.join(".o3k-upgrade-state.json"));
    let contents = match ctx.exec.read_file(&state_file) {
        Ok(contents) => contents,
        Err(_) => {
            return Check::new(
                "release.upgrade_state",
                Category::Release,
                CheckStatus::Pass,
                "no upgrade is in progress",
            );
        }
    };
    match serde_json::from_str::<crate::upgrade::UpgradeState>(&contents) {
        Ok(state) if state.phase == crate::upgrade::UpgradePhase::Committed => Check::new(
            "release.upgrade_state",
            Category::Release,
            CheckStatus::Pass,
            "the last upgrade committed cleanly",
        ),
        Ok(state) => Check::new(
            "release.upgrade_state",
            Category::Release,
            CheckStatus::Warn,
            format!("upgrade incomplete: phase {:?} recorded", state.phase),
        )
        .with_actions(vec![
            "sudo o3k upgrade".to_owned(),
            "sudo o3k rollback".to_owned(),
        ]),
        Err(error) => Check::new(
            "release.upgrade_state",
            Category::Release,
            CheckStatus::Warn,
            format!(
                "upgrade incomplete: the state file is unreadable: {}",
                crate::context::sanitize_error(&error.to_string())
            ),
        )
        .with_actions(vec![
            format!("ls -l {}", state_file.display()),
            "sudo o3k upgrade".to_owned(),
            "sudo o3k rollback".to_owned(),
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FakeDb, FakeExec, FakeHttp, context_with};

    fn context(exec: FakeExec) -> Context {
        context_with(exec, FakeHttp::healthy(), FakeDb::healthy(), true, true)
    }

    /// The healthy fixture: consistent hashes, a chain backup record, no
    /// state file.
    #[tokio::test]
    async fn all_three_checks_pass_on_the_healthy_fixture() {
        let ctx = context(FakeExec::healthy());
        let consistent = check_binary_set_consistent(&ctx).await;
        assert_eq!(consistent.status, CheckStatus::Pass);
        let backup = check_backup_available(&ctx).await;
        assert_eq!(backup.status, CheckStatus::Pass);
        let state = check_upgrade_state(&ctx).await;
        assert_eq!(state.status, CheckStatus::Pass);
    }

    /// A modified binary fails the set-consistency check.
    #[tokio::test]
    async fn binary_set_consistent_fails_on_modified_binary() {
        let mut exec = FakeExec::healthy();
        exec.digests.insert(
            "/usr/local/bin/o3k-compute".to_owned(),
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_owned(),
        );
        let check = check_binary_set_consistent(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("inconsistent"));
    }

    /// Mixed release versions in the sums file fail the check.
    #[tokio::test]
    async fn binary_set_consistent_fails_on_mixed_versions() {
        let mut exec = FakeExec::healthy();
        exec.files.insert(
            "/usr/local/share/o3k/SHA256SUMS".to_owned(),
            Ok(
                "0000000000000000000000000000000000000000000000000000000000000000  ./o3k-0.2.0-alpha.2/bin/o3kd\n\
                 0000000000000000000000000000000000000000000000000000000000000000  ./o3k-0.2.0-alpha.2/bin/o3k\n\
                 0000000000000000000000000000000000000000000000000000000000000000  ./o3k-0.3.0-alpha.1/bin/o3k-compute\n"
                    .to_owned(),
            ),
        );
        let check = check_binary_set_consistent(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("mixed"));
    }

    /// A missing SHA256SUMS reference is a WARN, not a FAIL.
    #[tokio::test]
    async fn binary_set_consistent_warns_without_reference() {
        let mut exec = FakeExec::healthy();
        exec.regular_files
            .retain(|path| path != "/usr/local/share/o3k/SHA256SUMS");
        let check = check_binary_set_consistent(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Warn);
    }

    /// Without a chain the backup check warns and recommends the upgrade.
    #[tokio::test]
    async fn backup_available_warns_without_chain() {
        let mut exec = FakeExec::healthy();
        exec.files.remove("/var/lib/o3k/backups/backup-chain.json");
        let check = check_backup_available(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check
                .recommended_actions
                .iter()
                .any(|action| action.contains("sudo o3k upgrade"))
        );
    }

    /// A tampered chain warns (the check never repairs).
    #[tokio::test]
    async fn backup_available_warns_on_invalid_chain() {
        let mut exec = FakeExec::healthy();
        exec.files.insert(
            "/var/lib/o3k/backups/backup-chain.json".to_owned(),
            Ok("{\"backups\":[{\"backup_id\":\"b1\",\"kind\":\"backup\"}]}".to_owned()),
        );
        let check = check_backup_available(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Warn);
    }

    /// An in-progress upgrade state warns with the exact next commands.
    #[tokio::test]
    async fn upgrade_state_warns_on_in_progress_state() {
        let mut exec = FakeExec::healthy();
        exec.files.insert(
            "/var/lib/o3k/.o3k-upgrade-state.json".to_owned(),
            Ok(
                "{\"source_version\":\"0.2.0-alpha.2\",\"target_version\":\"0.3.0-alpha.1\",\
                 \"phase\":\"SERVICES_STOPPED\",\"backup_id\":\"b1\",\
                 \"started_at\":\"2026-01-01T00:00:00Z\",\"updated_at\":\"2026-01-01T00:00:00Z\",\
                 \"rollback_performed\":false,\"doctor_status\":null}"
                    .to_owned(),
            ),
        );
        let check = check_upgrade_state(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check.summary.contains("upgrade incomplete"),
            "summary must say the upgrade is incomplete: {}",
            check.summary
        );
        assert!(
            check
                .recommended_actions
                .iter()
                .any(|a| a == "sudo o3k upgrade")
        );
        assert!(
            check
                .recommended_actions
                .iter()
                .any(|a| a == "sudo o3k rollback")
        );
    }

    /// A FAILED_UPGRADE state warns with the recovery commands.
    #[tokio::test]
    async fn upgrade_state_warns_on_failed_state() {
        let mut exec = FakeExec::healthy();
        exec.files.insert(
            "/var/lib/o3k/.o3k-upgrade-state.json".to_owned(),
            Ok(
                "{\"source_version\":\"0.2.0-alpha.2\",\"target_version\":\"0.3.0-alpha.1\",\
                 \"phase\":\"FAILED_UPGRADE\",\"backup_id\":\"b1\",\
                 \"started_at\":\"2026-01-01T00:00:00Z\",\"updated_at\":\"2026-01-01T00:00:00Z\",\
                 \"rollback_performed\":false,\"doctor_status\":null}"
                    .to_owned(),
            ),
        );
        let check = check_upgrade_state(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.summary.contains("upgrade incomplete"));
    }

    /// A COMMITTED state passes.
    #[tokio::test]
    async fn upgrade_state_passes_on_committed_state() {
        let mut exec = FakeExec::healthy();
        exec.files.insert(
            "/var/lib/o3k/.o3k-upgrade-state.json".to_owned(),
            Ok(
                "{\"source_version\":\"0.2.0-alpha.2\",\"target_version\":\"0.3.0-alpha.1\",\
                 \"phase\":\"COMMITTED\",\"backup_id\":\"b1\",\
                 \"started_at\":\"2026-01-01T00:00:00Z\",\"updated_at\":\"2026-01-01T00:00:00Z\",\
                 \"rollback_performed\":false,\"doctor_status\":\"healthy\"}"
                    .to_owned(),
            ),
        );
        let check = check_upgrade_state(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn version_fails_when_manifest_missing() {
        let mut exec = FakeExec::healthy();
        exec.regular_files
            .retain(|path| path != "/usr/local/share/o3k/release-manifest.json");
        let check = check_version(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("missing"));
    }

    #[tokio::test]
    async fn version_fails_when_manifest_has_no_version() {
        let mut exec = FakeExec::healthy();
        exec.files.insert(
            "/usr/local/share/o3k/release-manifest.json".to_owned(),
            Ok("{}".to_owned()),
        );
        let check = check_version(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn ownership_manifest_fails_when_missing() {
        let mut exec = FakeExec::healthy();
        exec.regular_files
            .retain(|path| path != "/usr/local/share/o3k/.o3k-installed");
        let check = check_ownership_manifest(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("missing"));
    }

    #[tokio::test]
    async fn ownership_manifest_fails_on_missing_entry() {
        let mut exec = FakeExec::healthy();
        exec.files.insert(
            "/usr/local/share/o3k/.o3k-installed".to_owned(),
            Ok(
                "o3k-installed-v1 prefix=/usr/local\nbin/o3kd\nshare/o3k/not-installed\n"
                    .to_owned(),
            ),
        );
        let check = check_ownership_manifest(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(
            check
                .details
                .as_deref()
                .is_some_and(|d| d.contains("share/o3k/not-installed"))
        );
    }

    #[tokio::test]
    async fn binary_hashes_fails_on_modified_binary() {
        let mut exec = FakeExec::healthy();
        exec.digests.insert(
            "/usr/local/bin/o3kd".to_owned(),
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_owned(),
        );
        let check = check_binary_hashes(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.summary.contains("modified installed binary: o3kd"));
    }

    #[tokio::test]
    async fn binary_hashes_warns_on_missing_reference() {
        let mut exec = FakeExec::healthy();
        exec.files.insert(
            "/usr/local/share/o3k/SHA256SUMS".to_owned(),
            Ok(String::new()),
        );
        let check = check_binary_hashes(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.summary.contains("reference hash"));
    }
}
