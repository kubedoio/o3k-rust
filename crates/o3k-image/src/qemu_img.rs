// HOST EXECUTION ADAPTER — not part of application domain.
//
// This module wraps `qemu-img` subprocess invocations with resource limits
// (timeout, output size, address space, open files). It is a bounded host/OS
// execution adapter that belongs conceptually in the infrastructure provider
// layer. The long-term target is extraction behind a provider port.
use std::{
    io::{self, Read},
    path::Path,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(crate) const QEMU_IMG_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const QEMU_IMG_MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
pub(crate) const QEMU_IMG_MAX_ADDRESS_SPACE_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const QEMU_IMG_MAX_OPEN_FILES: u64 = 128;
pub(crate) const O3K_TEST_QEMU_IMG_FAIL: &str = "O3K_TEST_QEMU_IMG_FAIL";

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

pub(crate) fn read_bounded_output<R: Read>(reader: R) -> io::Result<Vec<u8>> {
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

pub(crate) fn is_checksum(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
