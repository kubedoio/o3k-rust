//! Disposable real-command smoke for the Linux P11 provider.
//!
//! This proves only provider topology creation and cleanup on one host. It is
//! not guest traffic, policy, MTU, multi-host, or product-profile evidence.

use o3k_domain::{
    AddressRealm, EndpointLocation, FabricHostIdentity, FabricProviderKind, Ipv4Prefix,
    RealmEncapsulationBinding, RealmEndpointDirectory,
};
use o3k_network::{LinuxP11Config, LinuxP11FabricBackend, P11FabricBackend};
use std::{env, fs, path::PathBuf, process::Command};
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
        fabric_transport_ip: "198.18.0.1".parse()?,
        provider_version: "wireguard-v1".to_owned(),
        fabric_generation: 1,
        underlay_mtu: 1500,
        fabric_mtu: 1420,
    };
    let remote = FabricHostIdentity {
        host_id: "smoke-remote".to_owned(),
        public_key: "v+3Zvbhhd38dkie1myZTB4IyAIlHlM23ImWM9QXqnFM=".to_owned(),
        underlay_endpoint: "127.0.0.1:51821".to_owned(),
        fabric_transport_ip: "198.18.0.2".parse()?,
        provider_version: "wireguard-v1".to_owned(),
        fabric_generation: 1,
        underlay_mtu: 1500,
        fabric_mtu: 1420,
    };
    let binding = RealmEncapsulationBinding {
        fabric_domain_id: Uuid::from_u128(0x1300),
        realm_id: realm.id,
        provider_kind: FabricProviderKind::Geneve,
        provider_segment_id: 101,
        binding_generation: 1,
    };
    let plan = directory.compile_fabric_plan(&local, &[local.clone(), remote], 1400, &binding)?;
    let mut provider = LinuxP11FabricBackend::open(LinuxP11Config::for_root(&root))?;
    provider.apply(&plan)?;
    if !provider.observe(&plan)? {
        return Err("provider did not observe its applied state".into());
    }
    let geneve = Command::new("ip")
        .args([
            "netns",
            "exec",
            "o3k-fabric",
            "ip",
            "-d",
            "link",
            "show",
            "type",
            "geneve",
        ])
        .output()?;
    let geneve_output = String::from_utf8_lossy(&geneve.stdout);
    if !geneve.status.success()
        || !geneve_output.contains("geneve")
        || !geneve_output.contains("id 101")
        || !geneve_output.contains("remote 198.18.0.2")
    {
        return Err("provider did not realize the expected Geneve object".into());
    }
    let transport = Command::new("ip")
        .args([
            "netns",
            "exec",
            "o3k-fabric",
            "ip",
            "-4",
            "addr",
            "show",
            "dev",
            "wg-o3k",
        ])
        .output()?;
    if !transport.status.success()
        || !String::from_utf8_lossy(&transport.stdout).contains("198.18.0.1/32")
    {
        return Err("provider did not assign the local fabric transport address".into());
    }
    println!("p11-linux-smoke: host-transport-address=passed");
    println!("p11-linux-smoke: geneve-realization=passed");
    provider.remove(&plan)?;
    if !provider.observe_removed(&plan)? {
        return Err("provider did not observe cleanup".into());
    }
    fs::remove_dir_all(root)?;
    println!("p11-linux-smoke: topology-and-cleanup=passed");
    Ok(())
}
