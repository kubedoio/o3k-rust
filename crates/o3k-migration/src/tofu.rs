//! Explicit host-process adapter for the pinned OpenTofu handoff.

use crate::runner::RunnerError;
use std::process::Stdio;
use tokio::process::Command;

/// Invoke the pinned OpenTofu executable and accept only exit 0 as NO-OP.
/// This intentionally does not offer an in-memory shortcut.
pub async fn run_opentofu_noop(
    executable: &str,
    working_directory: &std::path::Path,
    environment: &[(String, String)],
) -> Result<String, RunnerError> {
    let mut command = Command::new(executable);
    command
        .current_dir(working_directory)
        .arg("plan")
        .arg("-detailed-exitcode")
        .arg("-input=false")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in environment {
        command.env(key, value);
    }
    let output = command
        .output()
        .await
        .map_err(|error| RunnerError::OpenTofu(error.to_string()))?;
    if output.status.code() != Some(0) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RunnerError::OpenTofu(format!(
            "plan was not NO-OP (exit {:?}): {}",
            output.status.code(),
            redact_process_output(&stderr)
        )));
    }
    Ok(redact_process_output(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn redact_process_output(value: &str) -> String {
    value
        .lines()
        .filter(|line| {
            !line.to_ascii_lowercase().contains("token")
                && !line.to_ascii_lowercase().contains("password")
                && !line.to_ascii_lowercase().contains("secret")
        })
        .take(20)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subprocess_exit_code_defines_noop() {
        let directory = std::env::temp_dir();
        let output = run_opentofu_noop("/bin/true", &directory, &[]).await;
        assert!(output.is_ok());
        let output = output.unwrap_or_default();
        assert!(output.is_empty());

        let error = run_opentofu_noop("/bin/false", &directory, &[]).await;
        assert!(matches!(error, Err(error) if error.to_string().contains("not NO-OP")));
    }
}
