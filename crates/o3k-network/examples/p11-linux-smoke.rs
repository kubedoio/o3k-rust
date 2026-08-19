//! Disposable real-command smoke for the Linux P11 provider.
//!
//! This proves only provider topology creation and cleanup on one host. It is
//! not guest traffic, policy, MTU, multi-host, or product-profile evidence.

use o3k_domain::{
    AddressRealm, EndpointLocation, FabricHostIdentity, Ipv4Prefix, RealmEndpointDirectory,
};
use o3k_network::{LinuxP11Config, LinuxP11FabricBackend, P11FabricBackend};
use std::{env, fs, path::PathBuf};
use uuid::Uuid;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = env::var_os("O3K_P11_SMOKE_ROOT")
        .map(PathBuf::from)
        .ok_or("O3K_P11_SMOKE_ROOT must name a disposable provider root")?;
    if root == std::path::Path::new("/") || root.as_os_str().is_empty() {
        return Err("refusing an unsafe smoke root".into());
    }
    let realm = AddressRealm {
        id: Uuid::from_u128(0x1100),
        project_id: "p11-smoke".to_owned(),
        prefix: Ipv4Prefix::new("10.250.1.0".parse()?, 24).ok_or("invalid prefix")?,
        overlapping_prefixes: false,
    };
    let directory = RealmEndpointDirectory::build(
        &realm,
        vec![EndpointLocation {
            endpoint_id: Uuid::from_u128(0x1200),
            project_id: realm.project_id.clone(),
            realm_id: realm.id,
            fixed_ip: "10.250.1.12".parse()?,
            mac: "02:00:00:00:01:12".to_owned(),
            selected_host: "smoke-remote".to_owned(),
            endpoint_generation: 1,
            placement_generation: 1,
        }],
        &[],
        1,
    )?;
    let local = FabricHostIdentity {
        host_id: "smoke-local".to_owned(),
        public_key: "local-public-key-is-not-a-peer".to_owned(),
        underlay_endpoint: "127.0.0.1:51820".to_owned(),
        provider_version: "wireguard-v1".to_owned(),
        fabric_generation: 1,
        underlay_mtu: 1500,
        fabric_mtu: 1420,
    };
    let remote = FabricHostIdentity {
        host_id: "smoke-remote".to_owned(),
        public_key: "v+3Zvbhhd38dkie1myZTB4IyAIlHlM23ImWM9QXqnFM=".to_owned(),
        underlay_endpoint: "127.0.0.1:51821".to_owned(),
        provider_version: "wireguard-v1".to_owned(),
        fabric_generation: 1,
        underlay_mtu: 1500,
        fabric_mtu: 1420,
    };
    let plan = directory.compile_fabric_plan(&local, &[local.clone(), remote], 1400)?;
    let mut provider = LinuxP11FabricBackend::open(LinuxP11Config::for_root(&root))?;
    provider.apply(&plan)?;
    if !provider.observe(&plan)? {
        return Err("provider did not observe its applied state".into());
    }
    provider.remove(&plan)?;
    if !provider.observe_removed(&plan)? {
        return Err("provider did not observe cleanup".into());
    }
    fs::remove_dir_all(root)?;
    println!("p11-linux-smoke: topology-and-cleanup=passed");
    Ok(())
}
