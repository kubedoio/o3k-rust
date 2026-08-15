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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FakeDb, FakeExec, FakeHttp, context_with};

    fn context(exec: FakeExec) -> Context {
        context_with(exec, FakeHttp::healthy(), FakeDb::healthy(), true, true)
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
