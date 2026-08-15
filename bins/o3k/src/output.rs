//! Output contract for `o3k doctor` (issue #617).
//!
//! The machine format is governed by
//! `contracts/o3k-doctor-output.schema.json`; the human format is a plain
//! sectioned listing designed for `sudo o3k doctor` on a terminal. Neither
//! format may ever contain secrets (passwords, tokens, private keys, or
//! unredacted configuration values).

use serde::Serialize;
use std::fmt::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

/// Status of a single diagnostic check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    NotApplicable,
}

impl fmt::Display for CheckStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => formatter.write_str("PASS"),
            Self::Warn => formatter.write_str("WARN"),
            Self::Fail => formatter.write_str("FAIL"),
            Self::NotApplicable => formatter.write_str("NOT_APPLICABLE"),
        }
    }
}

/// Section a check belongs to. The serialized value is the lowercase name;
/// the human section title is fixed by [`Category::section_title`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Host,
    Services,
    Control,
    Database,
    Identity,
    Compute,
    Libvirt,
    Network,
    Cloud,
    Security,
    Release,
}

impl Category {
    /// Fixed human-readable section title, in the fixed render order.
    #[must_use]
    pub fn section_title(self) -> &'static str {
        match self {
            Self::Host => "Host",
            Self::Services => "Services",
            Self::Control => "Control plane",
            Self::Database => "Database",
            Self::Identity => "Identity",
            Self::Compute => "Compute agent",
            Self::Libvirt => "Libvirt/KVM",
            Self::Network => "Networking/DHCP",
            Self::Cloud => "Cloud/API",
            Self::Security => "Security boundaries",
            Self::Release => "Installed release",
        }
    }

    /// All categories in the fixed render order.
    #[must_use]
    pub const fn all() -> [Self; 11] {
        [
            Self::Host,
            Self::Services,
            Self::Control,
            Self::Database,
            Self::Identity,
            Self::Compute,
            Self::Libvirt,
            Self::Network,
            Self::Cloud,
            Self::Security,
            Self::Release,
        ]
    }
}

/// One diagnostic check result.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: String,
    pub category: Category,
    pub status: CheckStatus,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recommended_actions: Vec<String>,
}

impl Check {
    /// Creates a check with no details and no recommended actions.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        category: Category,
        status: CheckStatus,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            category,
            status,
            summary: summary.into(),
            details: None,
            recommended_actions: Vec::new(),
        }
    }

    /// Attaches optional detail lines (plain text, may contain newlines).
    #[must_use]
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    /// Attaches recommended read-only diagnostic actions.
    #[must_use]
    pub fn with_actions(mut self, actions: Vec<String>) -> Self {
        self.recommended_actions = actions;
        self
    }
}

/// Aggregated verdict of one doctor run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OverallStatus {
    Healthy,
    Warning,
    Unhealthy,
}

impl fmt::Display for OverallStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Healthy => formatter.write_str("HEALTHY"),
            Self::Warning => formatter.write_str("WARNING"),
            Self::Unhealthy => formatter.write_str("UNHEALTHY"),
        }
    }
}

/// Complete machine-readable doctor output.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub version: String,
    pub overall_status: OverallStatus,
    pub timestamp: String,
    pub checks: Vec<Check>,
}

impl Report {
    /// Renders the fixed human format:
    ///
    /// ```text
    /// O3K Doctor v<version> <timestamp> UTC
    /// Host
    ///   PASS host.os_supported
    ///     <summary>
    ///     ...
    /// OVERALL: HEALTHY
    /// ```
    #[must_use]
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "O3K Doctor v{} {} UTC", self.version, self.timestamp);
        for category in Category::all() {
            let mut section = String::new();
            for check in &self.checks {
                if check.category != category {
                    continue;
                }
                let _ = writeln!(section, "  {} {}", check.status, check.id);
                let _ = writeln!(section, "    {}", check.summary);
                if let Some(details) = &check.details {
                    for line in details.lines() {
                        let _ = writeln!(section, "    {line}");
                    }
                }
                if matches!(check.status, CheckStatus::Warn | CheckStatus::Fail)
                    && !check.recommended_actions.is_empty()
                {
                    let _ = writeln!(section, "    Inspect:");
                    for action in &check.recommended_actions {
                        let _ = writeln!(section, "      {action}");
                    }
                }
            }
            if !section.is_empty() {
                let _ = writeln!(out, "{}", category.section_title());
                out.push_str(section.trim_end());
                let _ = writeln!(out);
            }
        }
        let _ = writeln!(out, "OVERALL: {}", self.overall_status);
        out
    }
}

/// Current UTC time as an RFC 3339 string with second precision (`...Z`),
/// computed from the system clock without external crates. Falls back to a
/// fixed epoch string when the clock predates 1970 (a clock far in the past
/// must never make doctor crash).
#[must_use]
pub fn now_utc_rfc3339() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => rfc3339_from_epoch_secs(duration.as_secs()),
        Err(_) => "1970-01-01T00:00:00Z".to_owned(),
    }
}

/// Formats a Unix epoch second count as an RFC 3339 UTC string with second
/// precision. Correct for the full supported range (roughly 1970-2100).
///
/// The civil date is derived with Howard Hinnant's `days_from_civil` inverse
/// (public-domain `date` algorithm), avoiding any leap-day edge cases in
/// hand-rolled year arithmetic.
#[must_use]
pub fn rfc3339_from_epoch_secs(epoch_secs: u64) -> String {
    let days = (epoch_secs / 86_400) as i64;
    let remainder = epoch_secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = remainder / 3_600;
    let minute = (remainder % 3_600) / 60;
    let second = remainder % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Converts a day count since 1970-01-01 into a proleptic Gregorian
/// (year, month, day). Inverse of Hinnant's `days_from_civil`.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let shifted = days_since_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The formatter must match RFC 3339 for a spread of known instants,
    /// including a leap day and a post-2000 value.
    #[test]
    fn rfc3339_matches_known_epoch_instants() {
        assert_eq!(rfc3339_from_epoch_secs(0), "1970-01-01T00:00:00Z");
        // 2000-02-29 (leap day) 00:00:00 UTC.
        assert_eq!(rfc3339_from_epoch_secs(951_782_400), "2000-02-29T00:00:00Z");
        // 2023-11-14 22:13:20 UTC.
        assert_eq!(
            rfc3339_from_epoch_secs(1_700_000_000),
            "2023-11-14T22:13:20Z"
        );
        // 2026-01-01 00:00:00 UTC.
        assert_eq!(
            rfc3339_from_epoch_secs(1_767_225_600),
            "2026-01-01T00:00:00Z"
        );
    }

    /// The human renderer must be deterministic and end with the verdict.
    #[test]
    fn human_renderer_groups_sections_and_ends_with_overall() {
        let report = Report {
            version: "0.0.0-test".to_owned(),
            overall_status: OverallStatus::Warning,
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            checks: vec![
                Check::new("host.os_supported", Category::Host, CheckStatus::Pass, "ok"),
                Check::new(
                    "database.integrity",
                    Category::Database,
                    CheckStatus::Warn,
                    "degraded",
                )
                .with_actions(vec!["journalctl -u o3kd -n 100".to_owned()]),
            ],
        };
        let rendered = report.render_human();
        assert!(rendered.starts_with("O3K Doctor v0.0.0-test 2026-01-01T00:00:00Z UTC\n"));
        assert!(rendered.contains("Host\n  PASS host.os_supported\n    ok\n"));
        assert!(rendered.contains(
            "Database\n  WARN database.integrity\n    degraded\n    Inspect:\n      journalctl -u o3kd -n 100\n"
        ));
        assert!(rendered.ends_with("OVERALL: WARNING\n"));
    }
}
