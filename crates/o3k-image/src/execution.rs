use std::{
    fs,
    io::{self, Read},
    path::Path,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(test)]
use std::process::ExitStatus;

use super::ImageError;

const QEMU_IMG_TIMEOUT: Duration = Duration::from_secs(30);
const QEMU_IMG_MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
const QEMU_IMG_MAX_ADDRESS_SPACE_BYTES: u64 = 1024 * 1024 * 1024;
const QEMU_IMG_MAX_OPEN_FILES: u64 = 128;

/// TEST-ONLY failpoint (issue #607): `1` makes every `run_qemu_img`
/// invocation fail with a deterministic bounded `io::Error` before any host
/// process is spawned. The setpriv `--reset-env` sandbox makes PATH-shim
/// injection impossible, so the failpoint must be read by this process
/// itself, before the sandbox is consulted — it is therefore honored
/// regardless of PATH. Disabled by default; any value other than exactly
/// `1` leaves behavior unchanged. Never a public API; used by the failure
/// matrix harness and the unit test only.
pub(crate) const O3K_TEST_QEMU_IMG_FAIL: &str = "O3K_TEST_QEMU_IMG_FAIL";

#[cfg(test)]
pub(crate) fn run_failpoint_child() -> io::Result<ExitStatus> {
    Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "tests::qemu_img_failpoint_env_armed_asserts_injected_failure",
        ])
        .env(O3K_TEST_QEMU_IMG_FAIL, "1")
        .spawn()?
        .wait()
}

pub(crate) fn verify_overlay(
    qemu_img: &Path,
    overlay: &Path,
    base: &Path,
) -> Result<(), ImageError> {
    let expected_base = fs::canonicalize(base).map_err(|_| ImageError::OverlayFailed)?;
    let output = run_qemu_img(
        qemu_img,
        [
            "info",
            "--output=json",
            overlay.to_str().ok_or(ImageError::OverlayFailed)?,
        ],
    )
    .map_err(|_| ImageError::OverlayFailed)?;
    if !output.status.success() {
        return Err(ImageError::OverlayFailed);
    }
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| ImageError::OverlayFailed)?;
    if info.get("format").and_then(serde_json::Value::as_str) != Some("qcow2") {
        return Err(ImageError::OverlayFailed);
    }

    let mut backing_paths = Vec::new();
    for field in ["backing-filename", "full-backing-filename"] {
        if let Some(backing) = info.get(field).and_then(serde_json::Value::as_str) {
            backing_paths.push(backing);
        }
    }
    if backing_paths.is_empty() {
        return Err(ImageError::OverlayFailed);
    }
    let overlay_parent = overlay.parent().ok_or(ImageError::OverlayFailed)?;
    for backing in backing_paths {
        let reported = Path::new(backing);
        let resolved = if reported.is_absolute() {
            reported.to_path_buf()
        } else {
            overlay_parent.join(reported)
        };
        if fs::canonicalize(resolved).map_err(|_| ImageError::OverlayFailed)? != expected_base {
            return Err(ImageError::OverlayFailed);
        }
    }
    Ok(())
}

pub(crate) fn overlay_virtual_size(qemu_img: &Path, overlay: &Path) -> Result<u64, ImageError> {
    let output = run_qemu_img(
        qemu_img,
        [
            "info",
            "--output=json",
            overlay.to_str().ok_or(ImageError::OverlayFailed)?,
        ],
    )
    .map_err(|_| ImageError::OverlayFailed)?;
    if !output.status.success() {
        return Err(ImageError::OverlayFailed);
    }
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| ImageError::OverlayFailed)?;
    if info.get("format").and_then(serde_json::Value::as_str) != Some("qcow2") {
        return Err(ImageError::OverlayFailed);
    }
    info.get("virtual-size")
        .and_then(serde_json::Value::as_u64)
        .ok_or(ImageError::OverlayFailed)
}

pub(crate) fn verify_image_format(
    qemu_img: &Path,
    image: &Path,
    expected: &str,
) -> Result<(), ImageError> {
    let output = run_qemu_img(
        qemu_img,
        [
            "info",
            "--output=json",
            image.to_str().ok_or(ImageError::FormatVerificationFailed)?,
        ],
    )
    .map_err(|_| ImageError::FormatVerificationFailed)?;
    if !output.status.success() {
        return Err(ImageError::FormatVerificationFailed);
    }
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| ImageError::FormatVerificationFailed)?;
    if info.get("format").and_then(serde_json::Value::as_str) != Some(expected) {
        return Err(ImageError::FormatVerificationFailed);
    }
    // Uploaded qcow2 bytes must be self-contained.  A backing reference (or
    // an external data file) would make later libvirt/qemu access depend on a
    // host path or protocol controlled by the uploader, and a nested chain
    // would evade the managed cache's ownership and digest checks.
    if expected == "qcow2"
        && [
            "backing-filename",
            "full-backing-filename",
            "backing-filename-format",
            "data-file",
            "data-file-raw",
        ]
        .iter()
        .any(|field| info.get(field).is_some_and(|value| !value.is_null()))
    {
        return Err(ImageError::FormatVerificationFailed);
    }
    Ok(())
}

/// Runs a read-only `qemu-img check` over a verified base so a truncated or
/// metadata-inconsistent qcow2 (extents beyond the end of the file, wrong
/// refcounts, overlapping structures) is rejected before an overlay is
/// derived from it and handed to libvirt.
///
/// `qemu-img check` without `-r` never repairs or writes the image. Its exit
/// code is 0 when the image is clean, 1 when only leaked clusters were found
/// (wasted space, no data corruption), and 2 when errors were found; any
/// other outcome (signal, helper failure) also fails closed.
pub(crate) fn verify_qcow2_consistency(qemu_img: &Path, image: &Path) -> Result<(), ImageError> {
    let output = run_qemu_img(
        qemu_img,
        [
            "check",
            image.to_str().ok_or(ImageError::FormatVerificationFailed)?,
        ],
    )
    .map_err(|_| ImageError::FormatVerificationFailed)?;
    if !matches!(output.status.code(), Some(0) | Some(1)) {
        return Err(ImageError::FormatVerificationFailed);
    }
    Ok(())
}

pub(crate) fn run_qemu_img<'a, I>(qemu_img: &Path, args: I) -> io::Result<Output>
where
    I: IntoIterator<Item = &'a str>,
{
    // Test-only failpoint (issue #607): read by this process before any
    // spawn, so it cannot be bypassed by PATH manipulation. The exact value
    // "1" injects a bounded, deterministic failure; unset or any other value
    // keeps the normal sandboxed invocation.
    if std::env::var_os(O3K_TEST_QEMU_IMG_FAIL).is_some_and(|value| value == "1") {
        return Err(io::Error::other(
            "qemu-img failure injected by O3K_TEST_QEMU_IMG_FAIL",
        ));
    }
    let args = args.into_iter().collect::<Vec<_>>();
    let setpriv = Path::new("/usr/bin/setpriv");
    if !setpriv.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "setpriv is required to sandbox qemu-img",
        ));
    }
    let prlimit = Path::new("/usr/bin/prlimit");
    if !prlimit.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "prlimit is required to bound qemu-img resources",
        ));
    }
    let mut command = Command::new(setpriv);
    command.args([
        "--no-new-privs",
        "--ambient-caps=-all",
        "--inh-caps=-all",
        "--reset-env",
        "--",
    ]);
    command.arg(prlimit);
    // RLIMIT_NPROC is enforced per real UID across the whole system, not per
    // process tree. A low --nproc bound therefore breaks the helper whenever
    // the service account already runs other threads (CI runners with parallel
    // test processes, or a busy o3k-compute account), because the helper cannot
    // create even one thread. Per-process bounds that actually hold are kept:
    // address space, open files, bounded output, and a hard timeout.
    command.args([
        format!("--as={QEMU_IMG_MAX_ADDRESS_SPACE_BYTES}"),
        format!("--nofile={QEMU_IMG_MAX_OPEN_FILES}"),
        "--".to_owned(),
    ]);
    command.arg(qemu_img);
    command.args(args);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("qemu-img stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("qemu-img stderr was not piped"))?;
    let stdout_reader = thread::spawn(move || read_bounded_output(stdout));
    let stderr_reader = thread::spawn(move || read_bounded_output(stderr));
    let deadline = Instant::now() + QEMU_IMG_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("qemu-img stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| io::Error::other("qemu-img stderr reader panicked"))??;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn read_bounded_output<R: Read>(reader: R) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(QEMU_IMG_MAX_OUTPUT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > QEMU_IMG_MAX_OUTPUT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "qemu-img output exceeded the safety bound",
        ));
    }
    Ok(bytes)
}
