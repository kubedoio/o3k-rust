//! Durable upgrade state: the phase machine record persisted at
//! `/var/lib/o3k/.o3k-upgrade-state.json` (mode 0600, atomic write).
//!
//! The file is host-local operational state in the same class as the
//! installer ownership manifest. Every phase persists its state BEFORE its
//! side effects; interruption + re-run resumes from the recorded phase (all
//! phase actions are safe to re-execute) or rolls back per the failure
//! policy.

use crate::output::now_utc_rfc3339;
use crate::version::ReleaseVersion;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The 12 upgrade phases plus the two terminal recovery phases. Derived
/// ordering puts the recovery phases after `Committed`; the engine only
/// compares against `BackupCreated` and the terminal phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UpgradePhase {
    Discovered,
    ReleaseDownloaded,
    ReleaseVerified,
    PreflightPassed,
    BackupCreated,
    ServicesStopped,
    BinariesInstalled,
    MigrationsApplied,
    ServicesStarted,
    HealthVerified,
    DoctorPassed,
    Committed,
    FailedUpgrade,
    RolledBack,
}

/// Whether the phase is a terminal recovery phase.
#[must_use]
pub fn is_terminal(phase: UpgradePhase) -> bool {
    matches!(
        phase,
        UpgradePhase::FailedUpgrade | UpgradePhase::RolledBack
    )
}

/// The persisted record of one upgrade attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradeState {
    pub source_version: ReleaseVersion,
    pub target_version: ReleaseVersion,
    pub phase: UpgradePhase,
    pub backup_id: Option<String>,
    pub started_at: String,
    pub updated_at: String,
    pub rollback_performed: bool,
    pub doctor_status: Option<String>,
}

impl UpgradeState {
    /// Creates a fresh DISCOVERED record for a new attempt.
    #[must_use]
    pub fn discovered(source_version: ReleaseVersion, target_version: ReleaseVersion) -> Self {
        let now = now_utc_rfc3339();
        Self {
            source_version,
            target_version,
            phase: UpgradePhase::Discovered,
            backup_id: None,
            started_at: now.clone(),
            updated_at: now,
            rollback_performed: false,
            doctor_status: None,
        }
    }

    /// Advances the record to a new phase (updating the timestamp).
    pub fn advance(&mut self, phase: UpgradePhase) {
        self.phase = phase;
        self.updated_at = now_utc_rfc3339();
    }

    /// Loads the persisted state. A missing file is `Ok(None)`; a file that
    /// exists but cannot be parsed is an error (never silently ignored).
    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot read the upgrade state: {error}")),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("the upgrade state file is invalid: {error}"))
    }

    /// Atomically persists the record with mode 0600.
    pub fn persist(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("cannot serialize the upgrade state: {error}"))?;
        write_atomic(path, &bytes)
    }

    /// Removes the persisted record (a missing file is success).
    pub fn clear(path: &Path) -> Result<(), String> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("cannot clear the upgrade state: {error}")),
        }
    }
}

/// Default state file path, honoring `O3K_UPGRADE_STATE_FILE` and falling
/// back to `<O3K_UPGRADE_DATA_DIR | /var/lib/o3k>/.o3k-upgrade-state.json`.
#[must_use]
pub fn default_state_path() -> PathBuf {
    if let Some(override_path) = env_path("O3K_UPGRADE_STATE_FILE") {
        return override_path;
    }
    default_data_dir().join(".o3k-upgrade-state.json")
}

/// Default data directory (`O3K_UPGRADE_DATA_DIR` or `/var/lib/o3k`).
#[must_use]
pub fn default_data_dir() -> PathBuf {
    env_path("O3K_UPGRADE_DATA_DIR").unwrap_or_else(|| PathBuf::from("/var/lib/o3k"))
}

/// Reads a non-empty path override from the process environment.
#[must_use]
pub fn env_path(key: &str) -> Option<PathBuf> {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

/// Writes bytes atomically: a temp file in the same directory (mode 0600),
/// fsync, rename over the destination, fsync the directory. The parent
/// directory is created (0700) when missing.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("state path has no parent: {}", path.display()))?;
    if !parent.exists() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        set_dir_mode_0700(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("state path has no file name: {}", path.display()))?;
    let temp = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
    let write_result = (|| -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        options.mode(0o600);
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        sync_parent_directory(parent)
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("cannot write {}: {error}", path.display()));
    }
    Ok(())
}

/// Sets a newly created directory to mode 0700 (best effort: a pre-existing
/// directory keeps its mode).
#[cfg(unix)]
fn set_dir_mode_0700(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("cannot stat {}: {error}", path.display()))?;
    if metadata.is_dir() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot set mode on {}: {error}", path.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_dir_mode_0700(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Fsyncs a directory so a rename inside it is durable.
#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> std::io::Result<()> {
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> std::io::Result<()> {
    Ok(())
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
        path.push(format!("o3k-state-test-{}-{n}", std::process::id()));
        path
    }

    fn version(major: u64, minor: u64, patch: u64, prerelease: &[&str]) -> ReleaseVersion {
        ReleaseVersion::new(
            major,
            minor,
            patch,
            prerelease.iter().map(|id| (*id).to_owned()).collect(),
        )
    }

    /// Persist/load round-trips every field.
    #[test]
    fn persist_and_load_round_trip() {
        let dir = temp_dir();
        let path = dir.join("state.json");
        let mut state = UpgradeState::discovered(
            version(0, 2, 0, &["alpha", "2"]),
            version(0, 3, 0, &["alpha", "1"]),
        );
        state.backup_id = Some("o3k-upgrade-0.2.0-alpha.2-0.3.0-alpha.1-1712345678".to_owned());
        state.doctor_status = Some("healthy".to_owned());
        assert!(
            state.persist(&path).is_ok(),
            "persist must succeed in the temp dir"
        );
        let loaded = UpgradeState::load(&path);
        let Some(loaded) = loaded.ok().flatten() else {
            assert!(false, "state must load back");
            return;
        };
        assert_eq!(loaded.backup_id, state.backup_id);
        assert_eq!(loaded.phase, state.phase);
        assert_eq!(loaded.source_version, state.source_version);
        assert_eq!(loaded.target_version, state.target_version);
        assert!(!loaded.rollback_performed);
        assert_eq!(loaded.doctor_status, state.doctor_status);
        assert!(!loaded.started_at.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A missing state file loads as None.
    #[test]
    fn missing_state_loads_as_none() {
        let dir = temp_dir();
        let path = dir.join("absent.json");
        let loaded = UpgradeState::load(&path);
        let Ok(loaded) = loaded else {
            assert!(false, "missing state must be Ok(None)");
            return;
        };
        assert!(loaded.is_none());
    }

    /// A corrupt state file is an error, never a silent fresh start.
    #[test]
    fn corrupt_state_is_an_error() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).ok();
        let path = dir.join("state.json");
        let _ = fs::write(&path, b"{not json");
        assert!(UpgradeState::load(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Clearing a missing file succeeds (idempotent).
    #[test]
    fn clear_is_idempotent() {
        let dir = temp_dir();
        let path = dir.join("absent.json");
        assert!(UpgradeState::clear(&path).is_ok());
        assert!(UpgradeState::clear(&path).is_ok());
    }

    /// The persisted file is mode 0600.
    #[cfg(unix)]
    #[test]
    fn persisted_state_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir();
        let path = dir.join("state.json");
        let state = UpgradeState::discovered(
            version(0, 2, 0, &["alpha", "2"]),
            version(0, 3, 0, &["alpha", "1"]),
        );
        assert!(state.persist(&path).is_ok());
        let mode = match fs::metadata(&path) {
            Ok(metadata) => metadata.permissions().mode() & 0o777,
            Err(error) => {
                assert!(false, "state must exist: {error}");
                return;
            }
        };
        assert_eq!(mode, 0o600, "state file must be private");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Terminal-phase classification drives the resume/refusal decisions.
    #[test]
    fn terminal_phases_are_classified() {
        assert!(is_terminal(UpgradePhase::FailedUpgrade));
        assert!(is_terminal(UpgradePhase::RolledBack));
        assert!(!is_terminal(UpgradePhase::Committed));
        assert!(!is_terminal(UpgradePhase::ServicesStopped));
    }

    /// BackupCreated is the first phase with side effects that survive an
    /// interruption, so resumption starts there.
    #[test]
    fn phase_ordering_places_backup_after_preflight() {
        assert!(UpgradePhase::BackupCreated > UpgradePhase::PreflightPassed);
        assert!(UpgradePhase::Committed > UpgradePhase::BackupCreated);
        assert!(UpgradePhase::FailedUpgrade > UpgradePhase::Committed);
    }
}
