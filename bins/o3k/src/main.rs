//! Thin binary entry point for the `o3k` operator CLI.
//!
//! Argument handling is a plain match on the first argument:
//! `doctor` drives the read-only check engine, `version` prints the binary
//! and installed release versions, and `upgrade`/`rollback` drive the
//! upgrade state machine (issue #626) through a current-thread tokio
//! runtime. The binary never panics: runtime build errors print to stderr
//! and exit 2.

use o3k::upgrade::engine::{UpgradeArgs, UpgradeOutcome, run_rollback, run_upgrade};
use o3k::upgrade::output::{UpgradeJson, UpgradeStatus};
use o3k::upgrade::runner::SystemUpgradeIo;
use o3k::{Context, ReleaseVersion, SqlxDoctorDb, SystemExec, SystemHttpClient, USAGE};
use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(first) = args.first().map(String::as_str) else {
        eprintln!("o3k: missing command\n\n{USAGE}");
        return ExitCode::from(2);
    };
    match first {
        "--help" | "-h" | "help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        "--version" | "-V" => {
            println!("o3k {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "version" => run_version(),
        "doctor" => run_doctor(&args[1..]),
        "upgrade" => run_upgrade_cli(&args[1..]),
        "rollback" => run_rollback_cli(&args[1..]),
        other => {
            eprintln!("o3k: unknown command '{other}'\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Builds the current-thread runtime every subcommand needs.
fn build_runtime() -> Result<tokio::runtime::Runtime, ExitCode> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            eprintln!("o3k: failed to build the async runtime: {error}");
            ExitCode::from(2)
        })
}

/// `o3k version`: the binary version plus the installed release version
/// read from the release manifest (`unknown` when unreadable).
fn run_version() -> ExitCode {
    println!("o3k {}", env!("CARGO_PKG_VERSION"));
    let installed = installed_version();
    println!("installed {installed}");
    ExitCode::SUCCESS
}

/// Reads the installed release version from the release manifest under the
/// standard prefix candidates.
fn installed_version() -> String {
    let prefix = std::env::var("O3K_UPGRADE_PREFIX")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/local"));
    let candidates = [prefix, std::path::PathBuf::from("/usr")];
    for candidate in &candidates {
        let manifest = candidate.join("share/o3k/release-manifest.json");
        let Ok(contents) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
            continue;
        };
        if let Some(version) = value
            .get("version")
            .and_then(serde_json::Value::as_str)
            .filter(|version| !version.is_empty())
        {
            return version.to_owned();
        }
    }
    "unknown".to_owned()
}

/// Parses `doctor`'s trailing arguments and runs the check engine.
fn run_doctor(trailing: &[String]) -> ExitCode {
    let mut json = false;
    for argument in trailing {
        match argument.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("o3k doctor: unknown option '{other}'\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let runtime = match build_runtime() {
        Ok(runtime) => runtime,
        Err(exit) => return exit,
    };
    let is_root = o3k::context::current_euid() == 0;
    let context = Context::load(
        Arc::new(SystemExec::new(is_root)),
        Arc::new(SystemHttpClient),
        Arc::new(SqlxDoctorDb),
    );
    let report = runtime.block_on(o3k::run_all(&context));
    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(serialized) => println!("{serialized}"),
            Err(error) => {
                eprintln!("o3k: failed to serialize the report: {error}");
                return ExitCode::from(2);
            }
        }
    } else {
        print!("{}", report.render_human());
    }
    if report.overall_status == o3k::OverallStatus::Healthy {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Parses `upgrade`'s trailing arguments and drives the upgrade engine.
fn run_upgrade_cli(trailing: &[String]) -> ExitCode {
    let mut requested = None;
    let mut check_only = false;
    let mut assume_yes = false;
    let mut json = false;
    let mut index = 0;
    while index < trailing.len() {
        match trailing[index].as_str() {
            "--to" => {
                index += 1;
                let Some(value) = trailing.get(index) else {
                    eprintln!("o3k upgrade: --to requires a version\n\n{USAGE}");
                    return ExitCode::from(2);
                };
                match value.parse::<ReleaseVersion>() {
                    Ok(version) => requested = Some(version),
                    Err(error) => {
                        eprintln!("o3k upgrade: invalid --to version '{value}': {error}");
                        return ExitCode::from(2);
                    }
                }
            }
            "--check" => check_only = true,
            "--yes" | "-y" => assume_yes = true,
            "--json" => json = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("o3k upgrade: unknown option '{other}'\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
        index += 1;
    }
    let runtime = match build_runtime() {
        Ok(runtime) => runtime,
        Err(exit) => return exit,
    };
    let io = SystemUpgradeIo::from_env();
    let args = UpgradeArgs {
        requested,
        check_only,
        assume_yes,
        doctor_retry_attempts: std::env::var("O3K_UPGRADE_DOCTOR_RETRY_ATTEMPTS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(5),
        doctor_retry_delay_ms: std::env::var("O3K_UPGRADE_DOCTOR_RETRY_DELAY_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(15_000),
    };
    let outcome = runtime.block_on(run_upgrade(&io, &args));
    render_upgrade_outcome("upgrade", outcome, json)
}

/// Parses `rollback`'s trailing arguments and drives the rollback engine.
fn run_rollback_cli(trailing: &[String]) -> ExitCode {
    let mut json = false;
    for argument in trailing {
        match argument.as_str() {
            "--yes" | "-y" => {}
            "--json" => json = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("o3k rollback: unknown option '{other}'\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let runtime = match build_runtime() {
        Ok(runtime) => runtime,
        Err(exit) => return exit,
    };
    let io = SystemUpgradeIo::from_env();
    let outcome = runtime.block_on(run_rollback(&io));
    render_upgrade_outcome("rollback", outcome, json)
}

/// Renders one engine outcome (human or JSON) and derives the exit code:
/// 0 for committed / rolled back / check passed, 1 for failed / blocked,
/// 2 for a serialization error.
fn render_upgrade_outcome(command: &str, outcome: UpgradeOutcome, json: bool) -> ExitCode {
    if json {
        let serialized = UpgradeJson::new(
            outcome.source_version.as_ref().map(ToString::to_string),
            outcome.target_version.as_ref().map(ToString::to_string),
            Some(outcome.phase),
            outcome.backup_id.clone(),
            outcome.status,
            outcome.rollback_performed,
            outcome.doctor_status.clone(),
        );
        match serde_json::to_string_pretty(&serialized) {
            Ok(serialized) => println!("{serialized}"),
            Err(error) => {
                eprintln!("o3k {command}: failed to serialize the result: {error}");
                return ExitCode::from(2);
            }
        }
        // The machine output stays on stdout; the human failure description
        // goes to stderr in both modes so operators and tests can read the
        // reason without parsing JSON.
        if let Some(error) = &outcome.error {
            eprintln!("o3k {command}: {error}");
        }
    } else {
        render_upgrade_human(command, &outcome);
    }
    match outcome.status {
        UpgradeStatus::Committed | UpgradeStatus::RolledBack | UpgradeStatus::CheckPassed => {
            ExitCode::SUCCESS
        }
        UpgradeStatus::Failed | UpgradeStatus::CheckBlocked => ExitCode::from(1),
    }
}

/// Human rendering of one engine outcome (errors go to stderr, the rest to
/// stdout; no secrets ever appear).
fn render_upgrade_human(command: &str, outcome: &UpgradeOutcome) {
    let versions = match (&outcome.source_version, &outcome.target_version) {
        (Some(source), Some(target)) => format!("{source} -> {target}"),
        _ => "unknown".to_owned(),
    };
    match outcome.status {
        UpgradeStatus::Committed => {
            println!("o3k {command}: upgrade committed: {versions}");
            if let Some(doctor) = &outcome.doctor_status {
                println!("o3k {command}: doctor overall: {doctor}");
            }
        }
        UpgradeStatus::RolledBack => {
            println!("o3k {command}: rollback completed: {versions}");
        }
        UpgradeStatus::CheckPassed => {
            println!("o3k {command}: check passed: {versions}");
        }
        UpgradeStatus::CheckBlocked => {
            if let Some(error) = &outcome.error {
                eprintln!("o3k {command}: check blocked: {error}");
            } else {
                eprintln!("o3k {command}: check blocked");
            }
        }
        UpgradeStatus::Failed => {
            if let Some(error) = &outcome.error {
                eprintln!("o3k {command}: {error}");
            } else {
                eprintln!("o3k {command}: failed");
            }
        }
    }
}
