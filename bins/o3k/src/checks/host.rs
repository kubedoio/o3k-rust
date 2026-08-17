//! Host-level checks: operating system, KVM device, and disk space.

use crate::checks::internal_failure;
use crate::context::Context;
use crate::output::{Category, Check, CheckStatus};
use std::path::Path;

/// One GiB in KiB, the unit `df -Pk` reports.
const GIB_KIB: u64 = 1_048_576;

/// Pass floor for free space on the data filesystem (5 GiB), warn below it
/// down to the 1 GiB hard fail floor (mirrors `packaging/preflight.sh`).
const PASS_FREE_KIB: u64 = 5 * GIB_KIB;
const FAIL_FREE_KIB: u64 = GIB_KIB;

/// `host.os_supported`: the host must run Ubuntu 24.04 or Debian 12 on
/// x86_64. A supported distribution at another version is a WARN; anything
/// else (or another architecture) is a FAIL.
pub async fn check(ctx: &Context) -> Check {
    let (id, version_id) = match ctx.exec.read_file(Path::new("/etc/os-release")) {
        Ok(contents) => {
            let mut id = None;
            let mut version_id = None;
            for line in contents.lines() {
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                let value = value.trim().trim_matches('"');
                match key {
                    "ID" => id = Some(value.to_owned()),
                    "VERSION_ID" => version_id = Some(value.to_owned()),
                    _ => {}
                }
            }
            (id, version_id)
        }
        Err(error) => {
            return internal_failure(
                "host.os_supported",
                Category::Host,
                "the host operating system",
                &error,
                vec!["cat /etc/os-release".to_owned()],
            );
        }
    };
    let arch_ok = std::env::consts::ARCH == "x86_64";
    let os_supported = matches!(
        (id.as_deref(), version_id.as_deref()),
        (Some("ubuntu"), Some("24.04")) | (Some("debian"), Some("12"))
    );
    let os_recognized = matches!(id.as_deref(), Some("ubuntu") | Some("debian"));
    let os_text = match (&id, &version_id) {
        (Some(id), Some(version)) => format!("{id} {version}"),
        (Some(id), None) => format!("{id} (unknown version)"),
        _ => "unknown operating system".to_owned(),
    };
    if arch_ok && os_supported {
        return Check::new(
            "host.os_supported",
            Category::Host,
            CheckStatus::Pass,
            format!("{os_text} on x86_64"),
        );
    }
    let mut findings = Vec::new();
    if !arch_ok {
        findings.push(format!(
            "unsupported architecture: {} (required: x86_64)",
            std::env::consts::ARCH
        ));
    }
    if !os_supported {
        if os_recognized {
            findings.push(format!("{os_text} (supported: ubuntu 24.04 or debian 12)"));
        } else {
            findings.push(format!("unsupported operating system: {os_text}"));
        }
    }
    let status = if arch_ok && os_recognized {
        CheckStatus::Warn
    } else {
        CheckStatus::Fail
    };
    Check::new(
        "host.os_supported",
        Category::Host,
        status,
        findings.join("; "),
    )
    .with_actions(vec![
        "cat /etc/os-release".to_owned(),
        "uname -m".to_owned(),
    ])
}

/// `host.kvm_device`: `/dev/kvm` must exist as a character device for nested
/// virtualization.
pub async fn check_kvm_device(ctx: &Context) -> Check {
    if ctx.is_kubernetes() {
        return Check::new(
            "host.kvm_device",
            Category::Host,
            CheckStatus::NotApplicable,
            "local /dev/kvm check is not applicable for Kubernetes control plane; compute agents run on external hypervisors",
        );
    }
    let kvm = Path::new("/dev/kvm");
    if ctx.exec.is_char_device(kvm) {
        return Check::new(
            "host.kvm_device",
            Category::Host,
            CheckStatus::Pass,
            "/dev/kvm is available",
        );
    }
    Check::new(
        "host.kvm_device",
        Category::Host,
        CheckStatus::Fail,
        "no KVM available for nested virtualization",
    )
    .with_actions(vec![
        "ls -l /dev/kvm".to_owned(),
        "lsmod | grep kvm".to_owned(),
    ])
}

/// Walks up from the data directory to its nearest existing ancestor,
/// mirroring `packaging/preflight.sh`'s space probe.
#[must_use]
fn nearest_existing_ancestor(path: &Path) -> std::path::PathBuf {
    let mut current = path.to_path_buf();
    while !current.exists() {
        let Some(parent) = current.parent() else {
            return std::path::PathBuf::from("/");
        };
        if parent == current {
            return std::path::PathBuf::from("/");
        }
        current = parent.to_path_buf();
    }
    current
}

/// `host.disk_space`: at least 5 GiB free on the data filesystem; 1-5 GiB is
/// a WARN, below 1 GiB a FAIL (the preflight hard floor).
pub async fn check_disk_space(ctx: &Context) -> Check {
    let probe = nearest_existing_ancestor(&ctx.data_dir);
    let available = match ctx.exec.df_avail_kib(&probe).await {
        Ok(available) => available,
        Err(error) => {
            return internal_failure(
                "host.disk_space",
                Category::Host,
                "the data filesystem free space",
                &error,
                vec![format!("df -h {}", probe.display())],
            );
        }
    };
    let action = format!("df -h {}", probe.display());
    if available >= PASS_FREE_KIB {
        return Check::new(
            "host.disk_space",
            Category::Host,
            CheckStatus::Pass,
            format!("{} GiB free on {}", available / GIB_KIB, probe.display()),
        );
    }
    if available >= FAIL_FREE_KIB {
        return Check::new(
            "host.disk_space",
            Category::Host,
            CheckStatus::Warn,
            format!(
                "only {} GiB free on {} (below the 5 GiB target)",
                available / GIB_KIB,
                probe.display()
            ),
        )
        .with_actions(vec![action]);
    }
    Check::new(
        "host.disk_space",
        Category::Host,
        CheckStatus::Fail,
        format!(
            "only {} GiB free on {} (below the 1 GiB minimum)",
            available / GIB_KIB,
            probe.display()
        ),
    )
    .with_actions(vec![action])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FakeDb, FakeExec, FakeHttp, context_with};

    fn context(exec: FakeExec) -> Context {
        context_with(exec, FakeHttp::healthy(), FakeDb::healthy(), true, true)
    }

    #[tokio::test]
    async fn os_supported_passes_on_ubuntu_2404() {
        let check = check(&context(FakeExec::healthy())).await;
        assert_eq!(check.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn os_supported_warns_on_other_version() {
        let mut exec = FakeExec::healthy();
        exec.files.insert(
            "/etc/os-release".to_owned(),
            Ok("ID=ubuntu\nVERSION_ID=\"22.04\"\n".to_owned()),
        );
        let check = check(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[tokio::test]
    async fn os_supported_fails_on_unknown_os() {
        let mut exec = FakeExec::healthy();
        exec.files.insert(
            "/etc/os-release".to_owned(),
            Ok("ID=fedora\nVERSION_ID=\"41\"\n".to_owned()),
        );
        let check = check(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn kvm_device_fails_without_kvm() {
        let mut exec = FakeExec::healthy();
        exec.char_devices.clear();
        let check = check_kvm_device(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn disk_space_fails_below_one_gib() {
        let mut exec = FakeExec::healthy();
        exec.df_kib = 512 * 1024;
        let check = check_disk_space(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
    }

    #[tokio::test]
    async fn disk_space_warns_between_one_and_five_gib() {
        let mut exec = FakeExec::healthy();
        exec.df_kib = 2 * GIB_KIB;
        let check = check_disk_space(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[tokio::test]
    async fn disk_space_fails_when_df_errors() {
        let mut exec = FakeExec::healthy();
        exec.df_error = Some("df: no such filesystem".to_owned());
        let check = check_disk_space(&context(exec)).await;
        assert_eq!(check.status, CheckStatus::Fail);
    }
}
