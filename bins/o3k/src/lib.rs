//! O3K operator CLI library.
//!
//! The `o3k` binary is the O3K operator command-line tool. Its first and
//! currently only subcommand is `o3k doctor`, a strictly read-only,
//! secret-safe diagnosis of a TestLab installation (issue #617).
//!
//! Design authority: `docs/plan/o3k-doctor.md`; machine output contract:
//! `contracts/o3k-doctor-output.schema.json`. Doctor never mutates host
//! state: SQLite opens read-only, `systemctl`/`virsh`/`ip`/`df` run only
//! state queries, and no repair command is ever executed.

pub mod checks;
pub mod context;
pub mod db;
pub mod engine;
pub mod output;
pub mod sys;
#[cfg(test)]
pub mod testutil;

pub use context::{Context, DoctorDb, Exec, HttpClient, HttpResponse, UnitState};
pub use db::SqlxDoctorDb;
pub use engine::{overall_status, run_all};
pub use output::{
    Category, Check, CheckStatus, OverallStatus, Report, now_utc_rfc3339, rfc3339_from_epoch_secs,
};
pub use sys::{SystemExec, SystemHttpClient};

/// Usage text for the CLI root.
pub const USAGE: &str = "\
o3k — O3K operator CLI

Usage:
  o3k doctor            run read-only installation diagnostics
  o3k doctor --json     machine-readable diagnostics (JSON on stdout)
  o3k --version         print the version and exit
  o3k --help            print this help and exit

Commands:
  doctor                diagnose the local O3K installation (read-only)

Exit codes:
  0                     diagnostics completed, overall healthy
  1                     diagnostics completed, warning or unhealthy
  2                     usage error";
