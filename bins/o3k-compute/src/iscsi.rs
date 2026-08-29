use super::*;

pub(super) fn read_iscsi_initiator() -> Option<String> {
    let contents = std::fs::read_to_string("/etc/iscsi/initiatorname.iscsi").ok()?;
    contents
        .lines()
        .find_map(|line| line.strip_prefix("InitiatorName="))
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Collects the compute connector description required by the Cinder
/// connector-update flow. Bounded, non-secret values only.
pub(super) fn collect_host_connector() -> Result<o3k_provider::ConnectorInfo, AgentError> {
    Ok(o3k_provider::ConnectorInfo {
        host: read_hostname(),
        ip: read_first_ip(),
        platform: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        os_type: std::env::consts::OS.to_owned(),
        multipath: false,
        initiator: read_iscsi_initiator(),
    })
}

pub(super) fn iscsiadm_command() -> std::process::Command {
    if std::env::var_os("O3K_ISCSIADM_SUDO").is_some_and(|value| value == "1") {
        let mut command = std::process::Command::new("sudo");
        command.args(["--non-interactive", "--", "/usr/sbin/iscsiadm"]);
        command
    } else {
        std::process::Command::new("iscsiadm")
    }
}

/// Logs into the iSCSI target and returns the observed host device path. A
/// missing iscsiadm is an explicit unsupported-connector failure; a successful
/// login without an observed device is an unknown outcome. The node record is
/// created when absent (os-brick sequence: show, then `--op new`), and
/// optional CHAP credentials are applied to the node session before login;
/// credentials are never logged (the redacted-message contract forbids
/// logging command arguments or raw iscsiadm output).
pub(super) fn iscsi_login(
    target_iqn: &str,
    target_portal: &str,
    _target_lun: u32,
    chap_auth: Option<(&str, &str)>,
) -> Result<Option<String>, AgentError> {
    if target_iqn.trim().is_empty() || target_portal.trim().is_empty() {
        return Err(AgentError::Protocol(
            "iscsi target is incomplete".to_owned(),
        ));
    }
    // The node record must exist before CHAP settings can be applied. Show
    // the node first (os-brick tolerates "no records found") and create the
    // record only when it is absent.
    let node_show = iscsiadm_command()
        .args(["--mode", "node", "-T", target_iqn, "-p", target_portal])
        .output();
    match node_show {
        Ok(output) if output.status.success() => {}
        Ok(_) | Err(_) => {
            let node_new = iscsiadm_command()
                .args([
                    "--mode",
                    "node",
                    "-T",
                    target_iqn,
                    "-p",
                    target_portal,
                    "--op",
                    "new",
                ])
                .output();
            match node_new {
                Ok(output) if output.status.success() => {}
                Ok(_) => {
                    // The redacted-message contract forbids raw command
                    // output: iscsiadm stderr can disclose target details.
                    tracing::debug!("iscsi node create command exited unsuccessfully");
                    return Err(AgentError::Protocol("iscsi node create failed".to_owned()));
                }
                Err(_) => {
                    return Err(AgentError::Protocol(
                        "iscsiadm is not available; iscsi connector is unsupported on this host"
                            .to_owned(),
                    ));
                }
            }
        }
    }
    if let Some((username, password)) = chap_auth {
        // Apply CHAP to the node session before login. Credentials are passed
        // as arguments and never logged or echoed into errors.
        let update = iscsiadm_command()
            .args([
                "--mode",
                "node",
                "-T",
                target_iqn,
                "-p",
                target_portal,
                "--op",
                "update",
                "-n",
                "node.session.auth.authmethod",
                "-v",
                "CHAP",
                "-n",
                "node.session.auth.username",
                "-v",
                username,
                "-n",
                "node.session.auth.password",
                "-v",
                password,
            ])
            .output();
        match update {
            Ok(output) if output.status.success() => {}
            Ok(_) => {
                tracing::debug!("iscsi CHAP update command exited unsuccessfully");
                return Err(AgentError::Protocol("iscsi CHAP update failed".to_owned()));
            }
            Err(_) => {
                return Err(AgentError::Protocol("iscsiadm is not available".to_owned()));
            }
        }
    }
    let login = iscsiadm_command()
        .args([
            "--mode",
            "node",
            "-T",
            target_iqn,
            "-p",
            target_portal,
            "--login",
        ])
        .output();
    match login {
        // Exit 15 means the session already exists, which is a successful
        // login (os-brick behavior).
        Ok(output) if output.status.success() || output.status.code() == Some(15) => {
            // Determine the host device path from the login session output.
            let stdout = String::from_utf8_lossy(&output.stdout);
            let session = stdout
                .lines()
                .find_map(|line| line.split("with session").nth(1))
                .map(|value| value.trim().to_owned());
            for _ in 0..10 {
                let device = iscsiadm_command()
                    .args(["--mode", "session", "-P", "3"])
                    .output()
                    .ok()
                    .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
                    .unwrap_or_default();
                let device = discover_iscsi_device(&device, target_iqn);
                if let Some(device) = device {
                    return Ok(Some(device));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            // A successful login with an unknown device path is an unknown
            // outcome, never an automatic failure.
            tracing::warn!(
                target_iqn,
                session = ?session,
                "iscsi login succeeded but device path is not yet observable"
            );
            Ok(None)
        }
        Ok(output) => {
            tracing::debug!(
                status = ?output.status,
                "iscsi login command exited unsuccessfully"
            );
            Err(AgentError::Protocol("iscsi login failed".to_owned()))
        }
        Err(_) => Err(AgentError::Protocol("iscsiadm is not available".to_owned())),
    }
}

pub(super) fn discover_iscsi_device(session_output: &str, target_iqn: &str) -> Option<String> {
    let lines: Vec<&str> = session_output.lines().collect();
    let mut in_target = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with("Target:") && trimmed.contains(target_iqn) {
            in_target = true;
            continue;
        }
        if in_target {
            if let Some(dev_name) = trimmed
                .strip_prefix("Attached scsi disk ")
                .and_then(|s| s.split_whitespace().next())
            {
                let path = format!("/dev/{dev_name}");
                if std::path::Path::new(&path).exists() {
                    return Some(path);
                }
            }
            if trimmed.starts_with("/dev/") {
                let dev_path = trimmed.split_whitespace().next().unwrap_or(trimmed);
                if std::path::Path::new(dev_path).exists() {
                    return Some(dev_path.to_owned());
                }
            }
            if !line.starts_with(' ') && !line.starts_with('\t') && trimmed.starts_with("Target:") {
                break;
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir("/dev/disk/by-path") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.contains(target_iqn) {
                return Some(entry.path().to_string_lossy().to_string());
            }
        }
    }
    None
}

pub(super) fn iscsi_logout(target_iqn: &str, target_portal: &str) -> Result<(), AgentError> {
    let _ = target_portal;
    let logout = iscsiadm_command()
        .args(["--mode", "node", "-T", target_iqn, "--logout"])
        .output();
    match logout {
        Ok(output) if output.status.success() => Ok(()),
        Ok(_) => Ok(()),
        Err(_) => Ok(()),
    }
}

/// Determines the next guest virtio block-device letter for a server. The
/// letter is stable for the volume on this server via a deterministic UUID
/// mapping over the bounded b..=z alphabet.
pub(super) fn attach_device_letter(resource_id: &str, volume_id: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    resource_id.hash(&mut hasher);
    volume_id.hash(&mut hasher);
    let index = (hasher.finish() % 24) as u8;
    let letter = (b'b' + index) as char;
    format!("vd{letter}")
}
