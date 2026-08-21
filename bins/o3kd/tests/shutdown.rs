#![cfg(unix)]

use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn assert_signal_shutdown(signal: &str) -> Result<(), Box<dyn std::error::Error>> {
    let probe = TcpListener::bind("127.0.0.1:0")?;
    let address = probe.local_addr()?;
    drop(probe);

    let mut child = Command::new(env!("CARGO_BIN_EXE_o3kd"))
        .args([
            "--listen-addr",
            &address.to_string(),
            "--data-dir",
            &format!("/tmp/o3kd-test-{}-{signal}", std::process::id()),
            "--log-filter",
            "off",
        ])
        .env(
            "O3K_NATIVE_CURSOR_HMAC_KEY",
            "test-native-cursor-key-0123456789abcdef",
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let startup_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < startup_deadline {
        if TcpStream::connect(address).is_ok() {
            break;
        }
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!("o3kd exited during startup: {status}")).into());
        }
        thread::sleep(Duration::from_millis(25));
    }
    if TcpStream::connect(address).is_err() {
        let _ = child.kill();
        return Err(io::Error::new(io::ErrorKind::TimedOut, "o3kd did not start").into());
    }
    let mut readiness = TcpStream::connect(address)?;
    readiness.set_read_timeout(Some(Duration::from_secs(2)))?;
    readiness.write_all(b"GET /readyz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut response = Vec::new();
    readiness.read_to_end(&mut response)?;
    let response = String::from_utf8_lossy(&response);
    if !response.starts_with("HTTP/1.1 200 OK") {
        let _ = child.kill();
        return Err(io::Error::other(format!("daemon is not ready: {response}")).into());
    }

    let signal_status = Command::new("kill")
        .args([format!("-{signal}"), child.id().to_string()])
        .status()?;
    if !signal_status.success() {
        let _ = child.kill();
        return Err(io::Error::other("failed to send shutdown signal").into());
    }

    let shutdown_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(io::Error::other(format!("o3kd shutdown failed: {status}")).into());
            }
            break;
        }
        if Instant::now() >= shutdown_deadline {
            let _ = child.kill();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "o3kd did not shut down").into());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

#[test]
fn sigterm_shuts_down_cleanly() -> Result<(), Box<dyn std::error::Error>> {
    assert_signal_shutdown("TERM")
}

#[test]
fn sigint_shuts_down_cleanly() -> Result<(), Box<dyn std::error::Error>> {
    assert_signal_shutdown("INT")
}
