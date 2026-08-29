use super::*;
use rustix::{
    fd::OwnedFd,
    process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal},
};
pub(super) fn pid_is_alive(pid: i32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    !stat
        .split_whitespace()
        .nth(2)
        .is_some_and(|state| state == "Z")
}

/// Ownership verification for the orphan reap: the process cmdline must
/// contain the O3K dhcp root path (the supervisor always launches dnsmasq
/// with `--conf-file=<root>/dnsmasq.conf`, so the root appears in the
/// argv). The canonicalized variant is accepted too, for hosts where the
/// agent and the spawned process disagree on symlinks. A read failure
/// (permissions, pid reuse race) is an unverifiable pid, never ownership.
pub(super) fn cmdline_contains_dhcp_root(
    pid: i32,
    root: &std::path::Path,
) -> Result<bool, std::io::Error> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline"))?;
    let cmdline = String::from_utf8_lossy(&raw);
    if cmdline.contains(root.to_string_lossy().as_ref()) {
        return Ok(true);
    }
    if let Ok(canonical) = std::fs::canonicalize(root) {
        return Ok(cmdline.contains(canonical.to_string_lossy().as_ref()));
    }
    Ok(false)
}

/// Opens a race-resistant Linux process handle after ownership verification.
/// Signals are sent through the pidfd, never through the reusable numeric PID.
pub(super) fn open_owned_pidfd(
    pid: i32,
    root: &std::path::Path,
    expected_starttime: u64,
) -> Result<OwnedFd, std::io::Error> {
    let raw_pid = pid;
    let pid = Pid::from_raw(raw_pid).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid process id")
    })?;
    // Acquire the stable process handle before inspecting /proc.  Checking
    // cmdline first leaves a PID-reuse window in which the verified process
    // can exit and the numeric PID can be assigned to a foreign process
    // before pidfd_open(), causing a signal to target the replacement.
    let pidfd = pidfd_open(pid, PidfdFlags::empty()).map_err(std::io::Error::from)?;
    if !cmdline_contains_dhcp_root(raw_pid, root)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "process command line is not O3K-owned",
        ));
    }
    // The spawn identity is the independent ownership proof: the kernel
    // start time of the process must equal the value recorded next to the
    // pidfile at spawn. A same-user process that forges the O3K dnsmasq
    // argv has a different start time; a process that inherited the numeric
    // PID after the owned dnsmasq exited has a different start time. Neither
    // may ever be signaled, so an identity mismatch is an unverifiable pid.
    if o3k_dhcp::process_starttime(raw_pid) != Some(expected_starttime) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "process start time does not match the recorded spawn identity",
        ));
    }
    Ok(pidfd)
}
