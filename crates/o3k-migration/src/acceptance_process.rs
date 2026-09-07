//! Host-process adapters used by the P14.9 evidence coordinator.
//!
//! These are bounded argv-only probes. They are kept outside the coordinator
//! so process execution remains an explicit execution boundary.

use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path, str::FromStr};
use tokio::process::Command;

pub async fn probe_postgres(database_url: &str, refs: &mut Vec<String>) -> bool {
    let options = match sqlx::postgres::PgConnectOptions::from_str(database_url) {
        Ok(options) => options,
        Err(_) => return false,
    };
    let pool = match sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
    {
        Ok(pool) => pool,
        Err(_) => return false,
    };
    let first = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&pool)
        .await
        .is_ok_and(|value| value == 1);
    pool.close().await;
    let reconnect = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(database_url)
        .await
        .is_ok();
    if reconnect {
        // The second connection is a separate liveness/reconnect proof.
        refs.push("postgresql:reconnect".into());
    }
    let passed = first && reconnect;
    if passed {
        refs.push("postgresql:select-1".into());
    }
    passed
}

pub async fn probe_guest_checksum(key: &Path, guest_ip: &str, expected_sha256: &str) -> bool {
    Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-i",
        ])
        .arg(key)
        .arg(format!("cirros@{guest_ip}"))
        .arg("sudo sha256sum /mnt/p14-volume/p14-checksum-input")
        .output()
        .await
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .next()
                    == Some(expected_sha256)
        })
}

pub async fn probe_toolchain(
    tofu: &str,
    provider: &Path,
    provider_version: &str,
    refs: &mut Vec<String>,
) -> (bool, BTreeMap<String, String>) {
    let tofu_result = Command::new(tofu).arg("version").output().await;
    let tofu_ok = tofu_result.is_ok_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains("OpenTofu v1.12.6")
    });
    let provider_hash = std::fs::read(provider)
        .ok()
        .map(|bytes| format!("{:x}", Sha256::digest(bytes)));
    let provider_ok = provider_hash.is_some() && provider_version == "3.4.0";
    if tofu_ok && provider_ok {
        refs.push("toolchain:openTofu-provider-identity".into());
    }
    let mut toolchain = BTreeMap::new();
    if tofu_ok {
        toolchain.insert("opentofu".into(), "1.12.6".into());
    }
    if let Some(hash) = provider_hash {
        toolchain.insert(
            "provider".into(),
            "terraform-provider-openstack/openstack 3.4.0".into(),
        );
        toolchain.insert("provider_sha256".into(), hash);
    }
    toolchain.insert("provider_modified".into(), "false".into());
    (tofu_ok && provider_ok, toolchain)
}
