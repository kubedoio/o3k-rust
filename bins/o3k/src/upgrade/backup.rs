//! Backup manifest and rollback chain (plan §6/§10).
//!
//! `BackupManifest` describes one O3K-created backup directory; the rollback
//! chain records every committed backup plus each performed rollback so a
//! second rollback is a detectable no-op. Both files live under the backup
//! root with the same permission class as the upgrade state (0600, atomic
//! writes).

use crate::upgrade::state::{default_data_dir, env_path, write_atomic};
use crate::version::ReleaseVersion;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// SHA-256 hex format: 64 lowercase hex digits.
fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// The manifest stored as `backup.json` inside one backup directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
    pub backup_id: String,
    pub source_version: ReleaseVersion,
    pub target_version: ReleaseVersion,
    pub source_commit: Option<String>,
    pub created_at: String,
    /// Binary name (for example `o3kd`) -> lowercase hex SHA-256.
    pub binary_sha256: BTreeMap<String, String>,
    pub schema_version_before: i64,
    pub db_restore_required_on_rollback: bool,
}

impl BackupManifest {
    /// Parses and validates serialized manifest bytes. Every field required
    /// by plan §6 must be present and well-formed; an invalid manifest is an
    /// error so a tampered backup can never be silently selected.
    pub fn validate(manifest_bytes: &[u8]) -> Result<Self, String> {
        let manifest: Self = serde_json::from_slice(manifest_bytes)
            .map_err(|error| format!("backup manifest is not valid JSON: {error}"))?;
        manifest.validate_values()?;
        Ok(manifest)
    }

    /// Field-level validation of an already-parsed manifest.
    pub fn validate_values(&self) -> Result<(), String> {
        if self.backup_id.trim().is_empty() {
            return Err("backup manifest has an empty backup_id".to_owned());
        }
        if self.created_at.trim().is_empty() {
            return Err("backup manifest has an empty created_at".to_owned());
        }
        if self.binary_sha256.is_empty() {
            return Err("backup manifest records no binary hashes".to_owned());
        }
        for (name, digest) in &self.binary_sha256 {
            if name.trim().is_empty() {
                return Err("backup manifest records a binary with an empty name".to_owned());
            }
            if !is_sha256_hex(digest) {
                return Err(format!(
                    "backup manifest records a malformed SHA-256 for {name}"
                ));
            }
        }
        if let Some(commit) = &self.source_commit
            && commit.trim().is_empty()
        {
            return Err("backup manifest records an empty source_commit".to_owned());
        }
        Ok(())
    }

    /// The backup directory for this record under the backup root.
    #[must_use]
    pub fn directory(&self, backup_root: &Path) -> PathBuf {
        backup_root.join(&self.backup_id)
    }
}

/// Kind of a rollback-chain entry. Backups are eligible rollback targets;
/// rollback records document that a restore happened and are never eligible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordKind {
    Backup,
    Rollback,
}

/// One rollback-chain entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackRecord {
    #[serde(flatten)]
    pub manifest: BackupManifest,
    pub kind: RecordKind,
}

/// The rollback chain stored as `backup-chain.json` under the backup root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RollbackChain {
    pub backups: Vec<RollbackRecord>,
}

impl RollbackChain {
    /// Loads the chain; a missing file is `Ok(None)`, an unparsable file an
    /// error.
    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot read the rollback chain: {error}")),
        };
        let chain: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("the rollback chain is invalid: {error}"))?;
        chain.validate()?;
        Ok(Some(chain))
    }

    /// Validates every record in the chain.
    pub fn validate(&self) -> Result<(), String> {
        for record in &self.backups {
            record
                .manifest
                .validate_values()
                .map_err(|error| format!("invalid chain record: {error}"))?;
        }
        Ok(())
    }

    /// Appends a record and persists the chain atomically (0600).
    pub fn append(path: &Path, record: RollbackRecord) -> Result<(), String> {
        record
            .manifest
            .validate_values()
            .map_err(|error| format!("refusing to append an invalid record: {error}"))?;
        let mut chain = Self::load(path)?.unwrap_or_default();
        // Deduplicate by backup id so a re-run of the COMMITTED phase can
        // never append the same backup twice.
        if let Some(existing) = chain
            .backups
            .iter()
            .find(|entry| entry.manifest.backup_id == record.manifest.backup_id)
            && existing.kind == record.kind
        {
            return Ok(());
        }
        chain.backups.push(record);
        let bytes = serde_json::to_vec(&chain)
            .map_err(|error| format!("cannot serialize the rollback chain: {error}"))?;
        write_atomic(path, &bytes)
    }

    /// The most recent eligible rollback target (a Backup-kind record).
    #[must_use]
    pub fn latest_eligible_backup(&self) -> Option<&BackupManifest> {
        self.backups
            .iter()
            .rev()
            .find(|record| record.kind == RecordKind::Backup)
            .map(|record| &record.manifest)
    }

    /// Whether a Backup-kind record with the given id exists.
    #[must_use]
    pub fn contains_backup(&self, backup_id: &str) -> bool {
        self.backups.iter().any(|record| {
            record.kind == RecordKind::Backup && record.manifest.backup_id == backup_id
        })
    }
}

/// Default backup root (`O3K_UPGRADE_BACKUP_DIR` or
/// `/var/lib/o3k/backups`).
#[must_use]
pub fn default_backup_dir() -> PathBuf {
    env_path("O3K_UPGRADE_BACKUP_DIR").unwrap_or_else(|| default_data_dir().join("backups"))
}

/// Default rollback chain path under the backup root.
#[must_use]
pub fn default_chain_path() -> PathBuf {
    default_backup_dir().join("backup-chain.json")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::assertions_on_constants)]
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> PathBuf {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("o3k-backup-test-{}-{n}", std::process::id()));
        path
    }

    fn valid_manifest() -> BackupManifest {
        BackupManifest {
            backup_id: "o3k-upgrade-0.2.0-alpha.2-0.3.0-alpha.1-1712345678".to_owned(),
            source_version: ReleaseVersion::new(0, 2, 0, vec!["alpha".to_owned(), "2".to_owned()]),
            target_version: ReleaseVersion::new(0, 3, 0, vec!["alpha".to_owned(), "1".to_owned()]),
            source_commit: Some("d6351864".to_owned()),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            binary_sha256: BTreeMap::from([(
                "o3kd".to_owned(),
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            )]),
            schema_version_before: 17,
            db_restore_required_on_rollback: false,
        }
    }

    fn serialize(manifest: &BackupManifest) -> Vec<u8> {
        match serde_json::to_vec(manifest) {
            Ok(bytes) => bytes,
            Err(error) => {
                assert!(serde_json::to_vec(manifest).is_ok(), "{error}");
                Vec::new()
            }
        }
    }

    /// A complete valid manifest parses and round-trips.
    #[test]
    fn valid_manifest_parses() {
        let manifest = valid_manifest();
        let bytes = serialize(&manifest);
        let parsed = BackupManifest::validate(&bytes);
        let Ok(parsed) = parsed else {
            assert!(false, "valid manifest must validate");
            return;
        };
        assert_eq!(parsed, manifest);
    }

    /// Missing required fields fail validation.
    #[test]
    fn missing_required_fields_fail() {
        for missing in [
            serde_json::json!({
                "source_version": "0.2.0-alpha.2",
                "target_version": "0.3.0-alpha.1",
                "source_commit": "abc",
                "created_at": "2026-01-01T00:00:00Z",
                "binary_sha256": {"o3kd": "a".repeat(64)},
                "schema_version_before": 17,
                "db_restore_required_on_rollback": false,
            }),
            serde_json::json!({
                "backup_id": "b1",
                "target_version": "0.3.0-alpha.1",
                "source_commit": "abc",
                "created_at": "2026-01-01T00:00:00Z",
                "binary_sha256": {"o3kd": "a".repeat(64)},
                "schema_version_before": 17,
                "db_restore_required_on_rollback": false,
            }),
            serde_json::json!({
                "backup_id": "b1",
                "source_version": "0.2.0-alpha.2",
                "source_commit": "abc",
                "created_at": "2026-01-01T00:00:00Z",
                "binary_sha256": {"o3kd": "a".repeat(64)},
                "schema_version_before": 17,
                "db_restore_required_on_rollback": false,
            }),
            serde_json::json!({
                "backup_id": "b1",
                "source_version": "0.2.0-alpha.2",
                "target_version": "0.3.0-alpha.1",
                "source_commit": "abc",
                "created_at": "2026-01-01T00:00:00Z",
                "schema_version_before": 17,
                "db_restore_required_on_rollback": false,
            }),
        ] {
            let bytes = missing.to_string().into_bytes();
            assert!(
                BackupManifest::validate(&bytes).is_err(),
                "manifest missing a required field must fail: {missing}"
            );
        }
    }

    /// Malformed SHA-256 digests fail validation.
    #[test]
    fn malformed_sha256_fails() {
        for digest in [
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            format!("{}z", "a".repeat(63)),
            "".to_owned(),
        ] {
            let mut manifest = valid_manifest();
            manifest
                .binary_sha256
                .insert("o3kd".to_owned(), digest.clone());
            assert!(
                BackupManifest::validate(&serialize(&manifest)).is_err(),
                "digest {digest:?} must fail validation"
            );
        }
    }

    /// Invalid version strings fail validation.
    #[test]
    fn invalid_version_strings_fail() {
        let manifest = serde_json::json!({
            "backup_id": "b1",
            "source_version": "not-a-version",
            "target_version": "0.3.0-alpha.1",
            "source_commit": "abc",
            "created_at": "2026-01-01T00:00:00Z",
            "binary_sha256": {"o3kd": "a".repeat(64)},
            "schema_version_before": 17,
            "db_restore_required_on_rollback": false,
        })
        .to_string()
        .into_bytes();
        assert!(BackupManifest::validate(&manifest).is_err());
    }

    /// An empty backup id fails validation.
    #[test]
    fn empty_backup_id_fails() {
        let mut manifest = valid_manifest();
        manifest.backup_id = String::new();
        assert!(BackupManifest::validate(&serialize(&manifest)).is_err());
        manifest.backup_id = "x".to_owned();
        manifest.created_at = String::new();
        assert!(BackupManifest::validate(&serialize(&manifest)).is_err());
    }

    /// Chain load returns None for a missing file and errors on garbage.
    #[test]
    fn chain_load_missing_and_corrupt() {
        let dir = temp_dir();
        let path = dir.join("backup-chain.json");
        let loaded = RollbackChain::load(&path);
        let Ok(loaded) = loaded else {
            assert!(false, "missing chain must be Ok(None)");
            return;
        };
        assert!(loaded.is_none());
        fs::create_dir_all(&dir).ok();
        let _ = fs::write(&path, b"{broken");
        assert!(RollbackChain::load(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Append persists records and deduplicates by (id, kind).
    #[test]
    fn chain_append_persists_and_deduplicates() {
        let dir = temp_dir();
        let path = dir.join("backup-chain.json");
        let record = RollbackRecord {
            manifest: valid_manifest(),
            kind: RecordKind::Backup,
        };
        assert!(RollbackChain::append(&path, record.clone()).is_ok());
        assert!(RollbackChain::append(&path, record.clone()).is_ok());
        let chain = RollbackChain::load(&path);
        let Some(chain) = chain.ok().flatten() else {
            assert!(false, "chain must load back");
            return;
        };
        assert_eq!(chain.backups.len(), 1, "duplicate appends must be ignored");
        assert!(chain.contains_backup(&record.manifest.backup_id));
        let rollback_record = RollbackRecord {
            manifest: valid_manifest(),
            kind: RecordKind::Rollback,
        };
        assert!(RollbackChain::append(&path, rollback_record).is_ok());
        let chain = RollbackChain::load(&path);
        let Some(chain) = chain.ok().flatten() else {
            assert!(false, "chain must load back");
            return;
        };
        assert_eq!(chain.backups.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Latest eligible backup skips rollback records.
    #[test]
    fn latest_eligible_backup_skips_rollback_records() {
        let manifest = valid_manifest();
        let chain = RollbackChain {
            backups: vec![
                RollbackRecord {
                    manifest: manifest.clone(),
                    kind: RecordKind::Backup,
                },
                RollbackRecord {
                    manifest: manifest.clone(),
                    kind: RecordKind::Rollback,
                },
            ],
        };
        let latest = chain.latest_eligible_backup();
        let Some(latest) = latest else {
            assert!(false, "a Backup record must be eligible");
            return;
        };
        assert_eq!(latest.backup_id, manifest.backup_id);
        let rollbacks_only = RollbackChain {
            backups: vec![RollbackRecord {
                manifest: manifest.clone(),
                kind: RecordKind::Rollback,
            }],
        };
        assert!(
            rollbacks_only.latest_eligible_backup().is_none(),
            "a chain without Backup records has no eligible rollback"
        );
        assert!(!rollbacks_only.contains_backup(&manifest.backup_id));
    }

    /// Invalid records cannot be appended to the chain.
    #[test]
    fn invalid_records_cannot_be_appended() {
        let dir = temp_dir();
        let path = dir.join("backup-chain.json");
        let mut manifest = valid_manifest();
        manifest
            .binary_sha256
            .insert("o3kd".to_owned(), "bad".to_owned());
        assert!(
            RollbackChain::append(
                &path,
                RollbackRecord {
                    manifest,
                    kind: RecordKind::Backup,
                }
            )
            .is_err()
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
