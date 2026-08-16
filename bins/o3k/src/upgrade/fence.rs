//! Upgrade-path fence (plan §4): pure, unit-tested decision logic.
//!
//! The fence refuses downgrades and same-version upgrades explicitly, then
//! enforces the channel family, the deployment profile, and the release's
//! `upgrade_from.min_version` floor. It never performs I/O.

use crate::version::ReleaseVersion;
use std::fmt;

/// Why an upgrade path was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceError {
    /// The target is older than the installed version.
    DowngradeRefused,
    /// The target equals the installed version.
    SameVersion,
    /// The installed version is older than the target release's
    /// `upgrade_from.min_version`.
    UnsupportedPathBelowMin,
    /// Installed and target deployment profiles differ.
    ProfileMismatch,
    /// Installed and target release channels differ.
    ChannelMismatch,
    /// A version string in the fence inputs could not be parsed.
    BadVersion,
}

impl fmt::Display for FenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::DowngradeRefused => {
                "downgrades are refused; reinstall the previous release instead"
            }
            Self::SameVersion => "the target release is already installed",
            Self::UnsupportedPathBelowMin => {
                "unsupported upgrade path: the installed version is older than the target's minimum; reinstall required"
            }
            Self::ProfileMismatch => "the target release belongs to a different deployment profile",
            Self::ChannelMismatch => "the target release belongs to a different release channel",
            Self::BadVersion => "a version in the upgrade fence is not a valid release version",
        };
        formatter.write_str(message)
    }
}

/// The inputs of the upgrade-path fence.
#[derive(Debug, Clone)]
pub struct UpgradeFence {
    /// The installed release version.
    pub source: ReleaseVersion,
    /// The requested/target release version.
    pub target: ReleaseVersion,
    /// The target release's `upgrade_from.min_version`, if declared.
    pub min_upgrade_from: Option<ReleaseVersion>,
    /// `(installed profile, target profile)`.
    pub profile: (String, String),
}

impl UpgradeFence {
    /// Builds the fence from parsed versions.
    #[must_use]
    pub fn new(
        source: ReleaseVersion,
        target: ReleaseVersion,
        min_upgrade_from: Option<ReleaseVersion>,
        profile: (String, String),
    ) -> Self {
        Self {
            source,
            target,
            min_upgrade_from,
            profile,
        }
    }

    /// Builds the fence from a raw `min_version` manifest string; a string
    /// that does not parse as a release version fails with [`FenceError::BadVersion`].
    pub fn from_manifest_values(
        source: ReleaseVersion,
        target: ReleaseVersion,
        installed_profile: &str,
        target_profile: &str,
        min_version: Option<&str>,
    ) -> Result<Self, FenceError> {
        let min_upgrade_from = match min_version {
            None => None,
            Some(text) => Some(text.parse().map_err(|_| FenceError::BadVersion)?),
        };
        Ok(Self {
            source,
            target,
            min_upgrade_from,
            profile: (installed_profile.to_owned(), target_profile.to_owned()),
        })
    }

    /// Decides whether the upgrade path is allowed.
    pub fn decide(&self) -> Result<(), FenceError> {
        if self.source == self.target {
            return Err(FenceError::SameVersion);
        }
        if self.target < self.source {
            return Err(FenceError::DowngradeRefused);
        }
        if self.source.channel() != self.target.channel() {
            return Err(FenceError::ChannelMismatch);
        }
        if self.profile.0 != self.profile.1 {
            return Err(FenceError::ProfileMismatch);
        }
        if let Some(minimum) = &self.min_upgrade_from
            && self.source < *minimum
        {
            return Err(FenceError::UnsupportedPathBelowMin);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::assertions_on_constants)]
    use super::*;

    fn version(major: u64, minor: u64, patch: u64, prerelease: &[&str]) -> ReleaseVersion {
        ReleaseVersion::new(
            major,
            minor,
            patch,
            prerelease.iter().map(|id| (*id).to_owned()).collect(),
        )
    }

    fn allowed(
        source: ReleaseVersion,
        target: ReleaseVersion,
        min_upgrade_from: Option<ReleaseVersion>,
        profile: (&str, &str),
    ) -> bool {
        UpgradeFence::new(
            source,
            target,
            min_upgrade_from,
            (profile.0.to_owned(), profile.1.to_owned()),
        )
        .decide()
        .is_ok()
    }

    /// A strictly newer target in the same channel/profile/min floor passes.
    #[test]
    fn allows_a_newer_same_channel_release() {
        // Exactly at the minimum is allowed.
        assert!(allowed(
            version(0, 2, 0, &["alpha", "2"]),
            version(0, 3, 0, &["alpha", "1"]),
            Some(version(0, 2, 0, &["alpha", "2"])),
            ("libvirt", "libvirt"),
        ));
        // No declared minimum is allowed.
        assert!(allowed(
            version(0, 2, 0, &["alpha", "2"]),
            version(0, 3, 0, &["alpha", "1"]),
            None,
            ("libvirt", "libvirt"),
        ));
    }

    /// Downgrades are refused explicitly, never silently.
    #[test]
    fn refuses_downgrades() {
        let error = UpgradeFence::new(
            version(0, 3, 0, &["alpha", "1"]),
            version(0, 2, 0, &["alpha", "2"]),
            None,
            ("libvirt".to_owned(), "libvirt".to_owned()),
        )
        .decide();
        assert_eq!(error, Err(FenceError::DowngradeRefused));
        assert!(
            error
                .as_ref()
                .err()
                .is_some_and(|kind| kind.to_string().contains("downgrade"))
        );
    }

    /// Same-version upgrades are refused with their own error kind.
    #[test]
    fn refuses_same_version() {
        let source = version(0, 3, 0, &["alpha", "1"]);
        let error = UpgradeFence::new(
            source.clone(),
            source,
            None,
            ("libvirt".to_owned(), "libvirt".to_owned()),
        )
        .decide();
        assert_eq!(error, Err(FenceError::SameVersion));
    }

    /// A source older than the target's minimum fails closed.
    #[test]
    fn refuses_sources_below_the_minimum() {
        let error = UpgradeFence::new(
            version(0, 1, 0, &["alpha", "1"]),
            version(0, 3, 0, &["alpha", "1"]),
            Some(version(0, 2, 0, &["alpha", "2"])),
            ("libvirt".to_owned(), "libvirt".to_owned()),
        )
        .decide();
        assert_eq!(error, Err(FenceError::UnsupportedPathBelowMin));
    }

    /// Profile mismatches are refused even when versions are newer.
    #[test]
    fn refuses_profile_mismatch() {
        let error = UpgradeFence::new(
            version(0, 2, 0, &["alpha", "2"]),
            version(0, 3, 0, &["alpha", "1"]),
            None,
            ("libvirt".to_owned(), "kubernetes".to_owned()),
        )
        .decide();
        assert_eq!(error, Err(FenceError::ProfileMismatch));
    }

    /// Channel mismatches (alpha to stable, alpha to beta) are refused.
    #[test]
    fn refuses_channel_mismatch() {
        let error = UpgradeFence::new(
            version(0, 2, 0, &["alpha", "2"]),
            version(0, 3, 0, &[]),
            None,
            ("libvirt".to_owned(), "libvirt".to_owned()),
        )
        .decide();
        assert_eq!(error, Err(FenceError::ChannelMismatch));
        let error = UpgradeFence::new(
            version(0, 2, 0, &["alpha", "2"]),
            version(0, 3, 0, &["beta", "1"]),
            None,
            ("libvirt".to_owned(), "libvirt".to_owned()),
        )
        .decide();
        assert_eq!(error, Err(FenceError::ChannelMismatch));
    }

    /// An unparsable `min_version` string yields BadVersion.
    #[test]
    fn unparsable_min_version_is_bad_version() {
        let error = UpgradeFence::from_manifest_values(
            version(0, 2, 0, &["alpha", "2"]),
            version(0, 3, 0, &["alpha", "1"]),
            "libvirt",
            "libvirt",
            Some("not-a-version"),
        );
        assert_eq!(error.err(), Some(FenceError::BadVersion));
    }

    /// A null `min_version` is accepted as "no floor".
    #[test]
    fn absent_min_version_is_no_floor() {
        let fence = UpgradeFence::from_manifest_values(
            version(0, 2, 0, &["alpha", "2"]),
            version(0, 3, 0, &["alpha", "1"]),
            "libvirt",
            "libvirt",
            None,
        );
        let Ok(fence) = fence else {
            assert!(false, "absent min_version must build a fence");
            return;
        };
        assert!(fence.decide().is_ok());
    }

    /// Cross-channel downgrades report DowngradeRefused (the explicit
    /// refusal), not ChannelMismatch.
    #[test]
    fn downgrade_check_precedes_channel_check() {
        let error = UpgradeFence::new(
            version(0, 3, 0, &["alpha", "1"]),
            version(0, 2, 0, &[]),
            None,
            ("libvirt".to_owned(), "libvirt".to_owned()),
        )
        .decide();
        assert_eq!(error, Err(FenceError::DowngradeRefused));
    }
}
