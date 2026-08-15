//! Thin binary entry point for the `o3k` operator CLI.
//!
//! Argument handling is a plain match on the first argument (per
//! `docs/plan/o3k-doctor.md`); `doctor` drives the check engine through a
//! current-thread tokio runtime. The binary never panics: runtime build
//! errors print to stderr and exit 2.

use o3k::{Context, SqlxDoctorDb, SystemExec, SystemHttpClient, USAGE};
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
        "doctor" => run_doctor(&args[1..]),
        other => {
            eprintln!("o3k: unknown command '{other}'\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
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
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("o3k: failed to build the async runtime: {error}");
            return ExitCode::from(2);
        }
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
