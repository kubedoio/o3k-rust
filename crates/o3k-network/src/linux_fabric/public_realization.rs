use crate::linux_fabric::public_execution::{PublicCommand, SystemPublicCommand};
use crate::public::PublicAddressError;
use o3k_domain::NetworkPlanIntent;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{ErrorKind, Write},
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

const OWNED_ADDRESS_FILE: &str = "owned-addresses.json";
const OWNED_BINDING_FILE: &str = "owned-bindings.json";

pub struct PublicAddressRealizer {
    root: PathBuf,
    uplink: String,
    pub(crate) command: Arc<dyn PublicCommand>,
    pub(crate) owned: bool,
    pub(crate) owned_addresses: Vec<Ipv4Addr>,
    pub(crate) owned_bindings: Vec<OwnedBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OwnedBinding {
    pub(crate) realm_id: Uuid,
    pub(crate) endpoint_id: Uuid,
    private_address: Ipv4Addr,
    public_address: Ipv4Addr,
}

pub(crate) const PUBLIC_TABLE: &str = "o3k_public";
pub(crate) const PUBLIC_MARKER: &str = "o3k-p9-public";

impl PublicAddressRealizer {
    pub fn open(root: impl Into<PathBuf>, uplink: String) -> Result<Self, PublicAddressError> {
        if uplink.is_empty() || uplink.len() > 15 {
            return Err(PublicAddressError::InvalidPool);
        }
        let root = root.into();
        fs::create_dir_all(&root)?;
        let owned = root.join("ownership").exists();
        let owned_addresses = load_owned_addresses(&root.join(OWNED_ADDRESS_FILE))?;
        let owned_bindings = load_owned_bindings(&root.join(OWNED_BINDING_FILE))?;
        validate_owned_bindings(&owned_bindings)?;
        Ok(Self {
            root,
            uplink,
            command: Arc::new(SystemPublicCommand),
            owned,
            owned_addresses,
            owned_bindings,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_command(
        root: impl Into<PathBuf>,
        uplink: String,
        command: Arc<dyn PublicCommand>,
    ) -> Result<Self, PublicAddressError> {
        let mut provider = Self::open(root, uplink)?;
        provider.command = command;
        Ok(provider)
    }

    pub fn apply(&mut self, intents: &[NetworkPlanIntent]) -> Result<(), PublicAddressError> {
        let endpoint_addresses: std::collections::HashMap<Uuid, Ipv4Addr> = intents
            .iter()
            .filter_map(|intent| match intent {
                NetworkPlanIntent::AddressAssignment {
                    endpoint_id,
                    address,
                    ..
                } => Some((*endpoint_id, *address)),
                _ => None,
            })
            .collect();
        let requested: Vec<(Uuid, Ipv4Addr)> = intents
            .iter()
            .filter_map(|intent| match intent {
                NetworkPlanIntent::PublicAddressBinding(binding) => {
                    Some((binding.endpoint_id, binding.public_address))
                }
                _ => None,
            })
            .collect();
        if requested.is_empty() {
            return Ok(());
        }
        let realm_id = plan_realm_id(intents)?;
        let mut bindings = self.owned_bindings.clone();
        for (endpoint_id, public_address) in requested {
            let binding = OwnedBinding {
                realm_id,
                endpoint_id,
                private_address: *endpoint_addresses
                    .get(&endpoint_id)
                    .ok_or(PublicAddressError::MissingEndpoint)?,
                public_address,
            };
            if let Some(existing) = bindings.iter_mut().find(|existing| {
                existing.realm_id == binding.realm_id && existing.endpoint_id == binding.endpoint_id
            }) {
                if existing.public_address != binding.public_address {
                    return Err(PublicAddressError::AssociationConflict);
                }
                *existing = binding;
            } else if bindings.iter().any(|existing| {
                existing.public_address == binding.public_address
                    || (existing.realm_id == binding.realm_id
                        && existing.endpoint_id == binding.endpoint_id)
            }) {
                return Err(PublicAddressError::AssociationConflict);
            } else {
                bindings.push(binding);
            }
        }
        for pair in bindings.windows(2) {
            if pair[0].public_address == pair[1].public_address {
                return Err(PublicAddressError::AssociationConflict);
            }
        }
        validate_owned_bindings(&bindings)?;
        self.realize_bindings(&bindings)
    }

    fn realize_bindings(&mut self, bindings: &[OwnedBinding]) -> Result<(), PublicAddressError> {
        let previous_addresses = self.owned_addresses.clone();
        let table_exists = self.ensure_foreign_safe()?;
        if table_exists && !self.owned {
            return Err(PublicAddressError::ForeignProviderState);
        }
        let mut desired_addresses: Vec<Ipv4Addr> = bindings
            .iter()
            .map(|binding| binding.public_address)
            .collect();
        desired_addresses.sort_unstable();
        desired_addresses.dedup();
        let (success, interface_addresses) = self
            .command
            .output("ip", &["-4", "addr", "show", "dev", &self.uplink])
            .map_err(PublicAddressError::Storage)?;
        if !success {
            return Err(PublicAddressError::ProviderCommandFailed);
        }
        for address in &desired_addresses {
            if address_present(&interface_addresses, *address)
                && !self.owned_addresses.contains(address)
            {
                return Err(PublicAddressError::ForeignProviderState);
            }
        }
        for address in previous_addresses
            .iter()
            .filter(|address| !desired_addresses.contains(address))
        {
            if address_present(&interface_addresses, *address)
                && !self
                    .command
                    .run(
                        "ip",
                        &["addr", "del", &format!("{address}/32"), "dev", &self.uplink],
                    )
                    .map_err(PublicAddressError::Storage)?
            {
                return Err(PublicAddressError::ProviderCommandFailed);
            }
        }
        fs::write(self.root.join("ownership"), PUBLIC_MARKER)?;
        self.owned = true;
        store_owned_addresses(&self.root.join(OWNED_ADDRESS_FILE), &desired_addresses)?;
        store_owned_bindings(&self.root.join(OWNED_BINDING_FILE), bindings)?;
        self.owned_addresses = desired_addresses.clone();
        self.owned_bindings = bindings.to_vec();
        if table_exists
            && !self
                .command
                .run("nft", &["delete", "table", "ip", PUBLIC_TABLE])
                .map_err(PublicAddressError::Storage)?
        {
            return Err(PublicAddressError::ProviderCommandFailed);
        }
        if bindings.is_empty() {
            let _ = fs::remove_file(self.root.join("ownership"));
            let _ = fs::remove_file(self.root.join(OWNED_ADDRESS_FILE));
            let _ = fs::remove_file(self.root.join(OWNED_BINDING_FILE));
            self.owned = false;
            self.owned_addresses.clear();
            self.owned_bindings.clear();
            return Ok(());
        }
        let table_args = [
            "add",
            "table",
            "ip",
            PUBLIC_TABLE,
            "{",
            "comment",
            &format!("\"{}\"", PUBLIC_MARKER),
            ";",
            "}",
        ];
        if !self
            .command
            .run("nft", &table_args)
            .map_err(PublicAddressError::Storage)?
        {
            return Err(PublicAddressError::ProviderCommandFailed);
        }
        let prerouting = [
            "add",
            "chain",
            "ip",
            PUBLIC_TABLE,
            "prerouting",
            "{",
            "type",
            "nat",
            "hook",
            "prerouting",
            "priority",
            "-100",
            ";",
            "policy",
            "accept",
            ";",
            "}",
        ];
        let postrouting = [
            "add",
            "chain",
            "ip",
            PUBLIC_TABLE,
            "postrouting",
            "{",
            "type",
            "nat",
            "hook",
            "postrouting",
            "priority",
            "100",
            ";",
            "policy",
            "accept",
            ";",
            "}",
        ];
        for address in &desired_addresses {
            if !address_present(&interface_addresses, *address)
                && !self
                    .command
                    .run(
                        "ip",
                        &["addr", "add", &format!("{address}/32"), "dev", &self.uplink],
                    )
                    .map_err(PublicAddressError::Storage)?
            {
                return Err(PublicAddressError::ProviderCommandFailed);
            }
        }
        if !self
            .command
            .run("nft", &prerouting)
            .map_err(PublicAddressError::Storage)?
            || !self
                .command
                .run("nft", &postrouting)
                .map_err(PublicAddressError::Storage)?
        {
            return Err(PublicAddressError::ProviderCommandFailed);
        }
        for binding in bindings {
            let private_address = binding.private_address.to_string();
            let public_address = binding.public_address.to_string();
            let uplink = format!("\"{}\"", self.uplink);
            let comment = format!(
                "\"{}:{}:{}\"",
                PUBLIC_MARKER, binding.realm_id, binding.endpoint_id
            );
            if !self
                .command
                .run(
                    "nft",
                    &[
                        "add",
                        "rule",
                        "ip",
                        PUBLIC_TABLE,
                        "prerouting",
                        "iifname",
                        &uplink,
                        "ip",
                        "daddr",
                        &public_address,
                        "dnat",
                        "to",
                        &private_address,
                        "comment",
                        &comment,
                    ],
                )
                .map_err(PublicAddressError::Storage)?
                || !self
                    .command
                    .run(
                        "nft",
                        &[
                            "add",
                            "rule",
                            "ip",
                            PUBLIC_TABLE,
                            "postrouting",
                            "ip",
                            "saddr",
                            &private_address,
                            "oifname",
                            &uplink,
                            "snat",
                            "to",
                            &public_address,
                            "comment",
                            &comment,
                        ],
                    )
                    .map_err(PublicAddressError::Storage)?
            {
                return Err(PublicAddressError::ProviderCommandFailed);
            }
        }
        Ok(())
    }

    pub fn remove_for_plan(
        &mut self,
        intents: &[NetworkPlanIntent],
    ) -> Result<(), PublicAddressError> {
        let targets: std::collections::HashSet<Uuid> = intents
            .iter()
            .filter_map(|intent| match intent {
                NetworkPlanIntent::PublicAddressBinding(binding) => Some(binding.endpoint_id),
                _ => None,
            })
            .collect();
        if targets.is_empty() {
            return Ok(());
        }
        let realm_id = plan_realm_id(intents)?;
        let retained: Vec<OwnedBinding> = self
            .owned_bindings
            .iter()
            .filter(|binding| {
                !(binding.realm_id == realm_id && targets.contains(&binding.endpoint_id))
            })
            .cloned()
            .collect();
        self.realize_bindings(&retained)
    }

    pub fn observe(&self) -> Result<bool, PublicAddressError> {
        let (success, output) = self
            .command
            .output("nft", &["list", "table", "ip", PUBLIC_TABLE])
            .map_err(PublicAddressError::Storage)?;
        Ok(success && output.contains(PUBLIC_MARKER))
    }

    pub fn remove(&mut self) -> Result<(), PublicAddressError> {
        if !self.owned && !self.root.join("ownership").exists() {
            return Ok(());
        }
        let (success, output) = self
            .command
            .output("nft", &["list", "table", "ip", PUBLIC_TABLE])
            .map_err(PublicAddressError::Storage)?;
        if success && !output.contains(PUBLIC_MARKER) {
            return Err(PublicAddressError::ForeignProviderState);
        }
        if success
            && !self
                .command
                .run("nft", &["delete", "table", "ip", PUBLIC_TABLE])
                .map_err(PublicAddressError::Storage)?
        {
            return Err(PublicAddressError::ProviderCommandFailed);
        }
        let (success, interface_addresses) = self
            .command
            .output("ip", &["-4", "addr", "show", "dev", &self.uplink])
            .map_err(PublicAddressError::Storage)?;
        if !success {
            self.owned = true;
            return Err(PublicAddressError::ProviderCommandFailed);
        }
        for address in &self.owned_addresses {
            if address_present(&interface_addresses, *address)
                && !self
                    .command
                    .run(
                        "ip",
                        &["addr", "del", &format!("{address}/32"), "dev", &self.uplink],
                    )
                    .map_err(PublicAddressError::Storage)?
            {
                self.owned = true;
                return Err(PublicAddressError::ProviderCommandFailed);
            }
        }
        let _ = fs::remove_file(self.root.join("ownership"));
        let _ = fs::remove_file(self.root.join(OWNED_ADDRESS_FILE));
        let _ = fs::remove_file(self.root.join(OWNED_BINDING_FILE));
        self.owned = false;
        self.owned_addresses.clear();
        self.owned_bindings.clear();
        Ok(())
    }

    fn ensure_foreign_safe(&self) -> Result<bool, PublicAddressError> {
        let (success, output) = self
            .command
            .output("nft", &["list", "table", "ip", PUBLIC_TABLE])
            .map_err(PublicAddressError::Storage)?;
        if success && !output.contains(PUBLIC_MARKER) {
            return Err(PublicAddressError::ForeignProviderState);
        }
        Ok(success)
    }
}

fn plan_realm_id(intents: &[NetworkPlanIntent]) -> Result<Uuid, PublicAddressError> {
    let realms = intents
        .iter()
        .filter_map(|intent| match intent {
            NetworkPlanIntent::AddressRealm { realm_id, .. } => Some(*realm_id),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    if realms.len() != 1 || realms.first().is_none_or(Uuid::is_nil) {
        return Err(PublicAddressError::MissingRealm);
    }
    realms
        .into_iter()
        .next()
        .ok_or(PublicAddressError::MissingRealm)
}

fn load_owned_addresses(path: &Path) -> Result<Vec<Ipv4Addr>, PublicAddressError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| PublicAddressError::CorruptState),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn load_owned_bindings(path: &Path) -> Result<Vec<OwnedBinding>, PublicAddressError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| PublicAddressError::CorruptState),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn validate_owned_bindings(bindings: &[OwnedBinding]) -> Result<(), PublicAddressError> {
    let mut identities = std::collections::BTreeSet::new();
    let mut public_addresses = std::collections::BTreeSet::new();
    for binding in bindings {
        if binding.realm_id.is_nil()
            || binding.endpoint_id.is_nil()
            || !identities.insert((binding.realm_id, binding.endpoint_id))
            || !public_addresses.insert(binding.public_address)
        {
            return Err(PublicAddressError::CorruptState);
        }
    }
    Ok(())
}

pub(crate) fn store_owned_addresses(
    path: &Path,
    addresses: &[Ipv4Addr],
) -> Result<(), PublicAddressError> {
    let bytes =
        serde_json::to_vec_pretty(addresses).map_err(|_| PublicAddressError::CorruptState)?;
    let temporary = path.with_extension("json.tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn store_owned_bindings(path: &Path, bindings: &[OwnedBinding]) -> Result<(), PublicAddressError> {
    let bytes =
        serde_json::to_vec_pretty(bindings).map_err(|_| PublicAddressError::CorruptState)?;
    let temporary = path.with_extension("json.tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn address_present(output: &str, address: Ipv4Addr) -> bool {
    output
        .split_whitespace()
        .filter_map(|token| token.split('/').next())
        .any(|token| token == address.to_string())
}
#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{PublicAddressAllocator, PublicAddressPool};
    use o3k_domain::{Ipv4Prefix, NetworkPlanIntent};
    use std::{
        io,
        sync::{Arc, Mutex},
    };

    struct FakePublicCommand {
        calls: Mutex<Vec<Vec<String>>>,
        listing: String,
        interface_listing: String,
        fail_on: Option<String>,
    }

    impl PublicCommand for FakePublicCommand {
        fn output(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)> {
            self.calls.lock().expect("calls").push(
                std::iter::once(program.to_owned())
                    .chain(args.iter().map(|arg| (*arg).to_owned()))
                    .collect(),
            );
            if program == "nft" {
                Ok((!self.listing.is_empty(), self.listing.clone()))
            } else {
                Ok((true, self.interface_listing.clone()))
            }
        }

        fn run(&self, program: &str, args: &[&str]) -> io::Result<bool> {
            let call = std::iter::once(program.to_owned())
                .chain(args.iter().map(|arg| (*arg).to_owned()))
                .collect::<Vec<_>>();
            let should_fail = self
                .fail_on
                .as_ref()
                .is_some_and(|needle| call.iter().any(|value| value == needle));
            self.calls.lock().expect("calls").push(call);
            if should_fail {
                return Ok(false);
            }
            Ok(true)
        }
    }

    fn allocator() -> PublicAddressAllocator {
        PublicAddressAllocator::open(
            std::env::temp_dir().join(format!("o3k-public-{}", Uuid::now_v7())),
            PublicAddressPool {
                prefix: Ipv4Prefix::new(Ipv4Addr::new(198, 51, 100, 0), 29).expect("prefix"),
                first_usable: Ipv4Addr::new(198, 51, 100, 2),
                last_usable: Ipv4Addr::new(198, 51, 100, 6),
            },
        )
        .expect("allocator")
    }

    #[test]
    fn allocation_is_idempotent_and_restartable() {
        let allocator = allocator();
        let first = allocator
            .allocate("project-a", "operation-1")
            .expect("allocation");
        let replay = allocator
            .allocate("project-a", "operation-1")
            .expect("replay");
        assert_eq!(first, replay);
        let reopened =
            PublicAddressAllocator::open(&allocator.root, allocator.pool.clone()).expect("reopen");
        assert_eq!(
            reopened.get("project-a", first.allocation_id).expect("get"),
            first
        );
    }

    #[test]
    fn cross_project_association_and_release_are_concealed() {
        let allocator = allocator();
        let binding = allocator
            .allocate("project-a", "operation-1")
            .expect("allocation");
        assert!(matches!(
            allocator.associate("project-b", binding.allocation_id, Uuid::now_v7()),
            Err(PublicAddressError::NotOwner)
        ));
        assert!(matches!(
            allocator.release("project-b", binding.allocation_id),
            Err(PublicAddressError::NotOwner)
        ));
    }

    #[test]
    fn association_is_idempotent_and_release_requires_disassociation() {
        let allocator = allocator();
        let endpoint = Uuid::now_v7();
        let binding = allocator
            .allocate("project-a", "operation-1")
            .expect("allocation");
        let associated = allocator
            .associate("project-a", binding.allocation_id, endpoint)
            .expect("associate");
        assert_eq!(
            allocator
                .associate("project-a", binding.allocation_id, endpoint)
                .expect("replay"),
            associated
        );
        assert!(matches!(
            allocator.release("project-a", binding.allocation_id),
            Err(PublicAddressError::InUse)
        ));
        allocator
            .disassociate("project-a", binding.allocation_id)
            .expect("disassociate");
        allocator
            .release("project-a", binding.allocation_id)
            .expect("release");
    }

    fn public_intents(endpoint_id: Uuid) -> Vec<NetworkPlanIntent> {
        vec![
            NetworkPlanIntent::AddressRealm {
                realm_id: Uuid::from_u128(1),
                prefix: Ipv4Prefix::new(Ipv4Addr::new(10, 0, 0, 0), 24).expect("realm prefix"),
                gateway: Ipv4Addr::new(10, 0, 0, 1),
            },
            NetworkPlanIntent::PublicAddressBinding(o3k_domain::PublicAddressBindingIntent {
                id: Uuid::from_u128(3),
                project_id: "project-a".to_owned(),
                public_address: Ipv4Addr::new(198, 51, 100, 2),
                endpoint_id,
                generation: 1,
            }),
        ]
    }

    #[test]
    fn public_realization_rejects_missing_endpoint_before_mutation() {
        let root = std::env::temp_dir().join(format!("o3k-public-provider-{}", Uuid::now_v7()));
        let command = Arc::new(FakePublicCommand {
            calls: Mutex::new(Vec::new()),
            listing: String::new(),
            interface_listing: String::new(),
            fail_on: None,
        });
        let mut provider = PublicAddressRealizer::with_command(
            &root,
            "eth0".to_owned(),
            Arc::clone(&command) as Arc<dyn PublicCommand>,
        )
        .expect("provider");
        assert!(matches!(
            provider.apply(&public_intents(Uuid::from_u128(9))),
            Err(PublicAddressError::MissingEndpoint)
        ));
        assert!(command.calls.lock().expect("calls").is_empty());
    }

    #[test]
    fn public_realization_rejects_missing_realm_before_mutation() {
        let root = std::env::temp_dir().join(format!("o3k-public-provider-{}", Uuid::now_v7()));
        let command = Arc::new(FakePublicCommand {
            calls: Mutex::new(Vec::new()),
            listing: String::new(),
            interface_listing: String::new(),
            fail_on: None,
        });
        let mut provider = PublicAddressRealizer::with_command(
            &root,
            "eth0".to_owned(),
            Arc::clone(&command) as Arc<dyn PublicCommand>,
        )
        .expect("provider");
        let endpoint = Uuid::from_u128(9);
        let mut intents = vec![NetworkPlanIntent::AddressAssignment {
            endpoint_id: endpoint,
            address: Ipv4Addr::new(10, 0, 0, 10),
            generation: 1,
        }];
        intents.push(public_intents(endpoint)[1].clone());
        assert!(matches!(
            provider.apply(&intents),
            Err(PublicAddressError::MissingRealm)
        ));
        assert!(command.calls.lock().expect("calls").is_empty());
    }

    #[test]
    fn public_realization_never_adopts_foreign_table() {
        let root = std::env::temp_dir().join(format!("o3k-public-provider-{}", Uuid::now_v7()));
        let command = Arc::new(FakePublicCommand {
            calls: Mutex::new(Vec::new()),
            listing: "table ip o3k_public { comment foreign; }".to_owned(),
            interface_listing: String::new(),
            fail_on: None,
        });
        let mut provider = PublicAddressRealizer::with_command(
            &root,
            "eth0".to_owned(),
            Arc::clone(&command) as Arc<dyn PublicCommand>,
        )
        .expect("provider");
        let endpoint = Uuid::from_u128(9);
        let intents = public_intents(endpoint);
        let intents = [
            intents[0].clone(),
            NetworkPlanIntent::AddressAssignment {
                endpoint_id: endpoint,
                address: Ipv4Addr::new(10, 0, 0, 2),
                generation: 1,
            },
            intents[1].clone(),
        ];
        assert!(matches!(
            provider.apply(&intents),
            Err(PublicAddressError::ForeignProviderState)
        ));
        assert_eq!(command.calls.lock().expect("calls").len(), 1);
    }

    #[test]
    fn public_realization_rebuilds_owned_table_after_restart() {
        let root = std::env::temp_dir().join(format!("o3k-public-provider-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("ownership"), PUBLIC_MARKER).expect("ownership");
        let command = Arc::new(FakePublicCommand {
            calls: Mutex::new(Vec::new()),
            listing: format!("table ip {PUBLIC_TABLE} {{ comment {PUBLIC_MARKER}; }}"),
            interface_listing: String::new(),
            fail_on: None,
        });
        let endpoint = Uuid::from_u128(9);
        let mut provider = PublicAddressRealizer::with_command(
            &root,
            "eth0".to_owned(),
            Arc::clone(&command) as Arc<dyn PublicCommand>,
        )
        .expect("provider");
        let mut intents = vec![NetworkPlanIntent::AddressAssignment {
            endpoint_id: endpoint,
            address: Ipv4Addr::new(10, 0, 0, 2),
            generation: 1,
        }];
        intents.extend(public_intents(endpoint));

        provider.apply(&intents).expect("rebuild");
        let calls = command.calls.lock().expect("calls");
        assert!(
            calls
                .iter()
                .any(|call| call == &["nft", "delete", "table", "ip", PUBLIC_TABLE])
        );
        assert!(calls.iter().any(|call| call
            == &[
                "nft",
                "add",
                "table",
                "ip",
                PUBLIC_TABLE,
                "{",
                "comment",
                &format!("\"{}\"", PUBLIC_MARKER),
                ";",
                "}"
            ]));
        assert!(
            calls
                .iter()
                .any(|call| { call == &["ip", "addr", "add", "198.51.100.2/32", "dev", "eth0"] })
        );
    }

    #[test]
    fn public_realization_preserves_unrelated_bindings_during_apply_and_remove() {
        let root = std::env::temp_dir().join(format!("o3k-public-provider-{}", Uuid::now_v7()));
        let command = Arc::new(FakePublicCommand {
            calls: Mutex::new(Vec::new()),
            listing: String::new(),
            interface_listing: String::new(),
            fail_on: None,
        });
        let mut provider = PublicAddressRealizer::with_command(
            &root,
            "eth0".to_owned(),
            Arc::clone(&command) as Arc<dyn PublicCommand>,
        )
        .expect("provider");
        let first = Uuid::from_u128(9);
        let second = Uuid::from_u128(10);
        let first_intents = [
            NetworkPlanIntent::AddressRealm {
                realm_id: Uuid::from_u128(1),
                prefix: Ipv4Prefix::new(Ipv4Addr::new(10, 0, 0, 0), 24).expect("realm prefix"),
                gateway: Ipv4Addr::new(10, 0, 0, 1),
            },
            NetworkPlanIntent::AddressAssignment {
                endpoint_id: first,
                address: Ipv4Addr::new(10, 0, 0, 2),
                generation: 1,
            },
            public_intents(first)[1].clone(),
        ];
        provider.apply(&first_intents).expect("first binding");
        let second_intents = [
            NetworkPlanIntent::AddressRealm {
                realm_id: Uuid::from_u128(2),
                prefix: Ipv4Prefix::new(Ipv4Addr::new(10, 0, 0, 0), 24).expect("realm prefix"),
                gateway: Ipv4Addr::new(10, 0, 0, 1),
            },
            NetworkPlanIntent::AddressAssignment {
                endpoint_id: second,
                address: Ipv4Addr::new(10, 0, 0, 3),
                generation: 1,
            },
            NetworkPlanIntent::PublicAddressBinding(o3k_domain::PublicAddressBindingIntent {
                id: Uuid::from_u128(4),
                project_id: "project-a".to_owned(),
                public_address: Ipv4Addr::new(198, 51, 100, 3),
                endpoint_id: second,
                generation: 1,
            }),
        ];
        provider.apply(&second_intents).expect("second binding");
        assert_eq!(provider.owned_bindings.len(), 2);
        provider
            .remove_for_plan(&first_intents)
            .expect("remove first");
        assert_eq!(provider.owned_bindings.len(), 1);
        assert_eq!(provider.owned_bindings[0].endpoint_id, second);
        let stored = fs::read_to_string(root.join(OWNED_BINDING_FILE)).expect("stored bindings");
        assert!(stored.contains(&second.to_string()));
        assert!(!stored.contains(&first.to_string()));
    }

    #[test]
    fn public_realization_fails_when_nat_chain_creation_fails() {
        let root = std::env::temp_dir().join(format!("o3k-public-provider-{}", Uuid::now_v7()));
        let command = Arc::new(FakePublicCommand {
            calls: Mutex::new(Vec::new()),
            listing: String::new(),
            interface_listing: String::new(),
            fail_on: Some("prerouting".to_owned()),
        });
        let mut provider = PublicAddressRealizer::with_command(
            &root,
            "eth0".to_owned(),
            Arc::clone(&command) as Arc<dyn PublicCommand>,
        )
        .expect("provider");
        let endpoint = Uuid::from_u128(9);
        let mut intents = vec![NetworkPlanIntent::AddressAssignment {
            endpoint_id: endpoint,
            address: Ipv4Addr::new(10, 0, 0, 2),
            generation: 1,
        }];
        intents.extend(public_intents(endpoint));

        assert!(matches!(
            provider.apply(&intents),
            Err(PublicAddressError::ProviderCommandFailed)
        ));
        assert!(root.join("ownership").exists());
    }

    #[test]
    fn overlapping_private_addresses_remain_realm_scoped() {
        let root = std::env::temp_dir().join(format!("o3k-public-provider-{}", Uuid::now_v7()));
        let command = Arc::new(FakePublicCommand {
            calls: Mutex::new(Vec::new()),
            listing: String::new(),
            interface_listing: String::new(),
            fail_on: None,
        });
        let mut provider = PublicAddressRealizer::with_command(
            &root,
            "eth0".to_owned(),
            Arc::clone(&command) as Arc<dyn PublicCommand>,
        )
        .expect("provider");
        let endpoint_a = Uuid::from_u128(20);
        let endpoint_b = Uuid::from_u128(21);
        let realm_a = Uuid::from_u128(30);
        let realm_b = Uuid::from_u128(31);
        let realm_intent = |realm_id| NetworkPlanIntent::AddressRealm {
            realm_id,
            prefix: Ipv4Prefix::new(Ipv4Addr::new(10, 0, 0, 0), 24).expect("realm prefix"),
            gateway: Ipv4Addr::new(10, 0, 0, 1),
        };
        let binding_intent = |endpoint_id, public_address| {
            NetworkPlanIntent::PublicAddressBinding(o3k_domain::PublicAddressBindingIntent {
                id: Uuid::now_v7(),
                project_id: "project-a".to_owned(),
                public_address,
                endpoint_id,
                generation: 1,
            })
        };
        provider
            .apply(&[
                realm_intent(realm_a),
                NetworkPlanIntent::AddressAssignment {
                    endpoint_id: endpoint_a,
                    address: Ipv4Addr::new(10, 0, 0, 10),
                    generation: 1,
                },
                binding_intent(endpoint_a, Ipv4Addr::new(198, 51, 100, 2)),
            ])
            .expect("realm A binding");
        provider
            .apply(&[
                realm_intent(realm_b),
                NetworkPlanIntent::AddressAssignment {
                    endpoint_id: endpoint_b,
                    address: Ipv4Addr::new(10, 0, 0, 10),
                    generation: 1,
                },
                binding_intent(endpoint_b, Ipv4Addr::new(198, 51, 100, 3)),
            ])
            .expect("realm B binding");

        assert_eq!(provider.owned_bindings.len(), 2);
        assert!(
            provider
                .owned_bindings
                .iter()
                .any(|binding| binding.realm_id == realm_a && binding.endpoint_id == endpoint_a)
        );
        assert!(
            provider
                .owned_bindings
                .iter()
                .any(|binding| binding.realm_id == realm_b && binding.endpoint_id == endpoint_b)
        );
        let calls = command.calls.lock().expect("calls");
        assert!(calls.iter().any(|call| {
            call.iter().any(|value| value == "198.51.100.2")
                && call
                    .iter()
                    .any(|value| value.contains(&realm_a.to_string()))
        }));
        assert!(calls.iter().any(|call| {
            call.iter().any(|value| value == "198.51.100.3")
                && call
                    .iter()
                    .any(|value| value.contains(&realm_b.to_string()))
        }));
    }

    #[test]
    fn public_realization_never_adopts_foreign_uplink_address() {
        let root = std::env::temp_dir().join(format!("o3k-public-provider-{}", Uuid::now_v7()));
        let command = Arc::new(FakePublicCommand {
            calls: Mutex::new(Vec::new()),
            listing: String::new(),
            interface_listing: "    inet 198.51.100.2/32 scope global eth0".to_owned(),
            fail_on: None,
        });
        let mut provider = PublicAddressRealizer::with_command(
            &root,
            "eth0".to_owned(),
            Arc::clone(&command) as Arc<dyn PublicCommand>,
        )
        .expect("provider");
        let endpoint = Uuid::from_u128(9);
        let mut intents = vec![NetworkPlanIntent::AddressAssignment {
            endpoint_id: endpoint,
            address: Ipv4Addr::new(10, 0, 0, 2),
            generation: 1,
        }];
        intents.extend(public_intents(endpoint));

        assert!(matches!(
            provider.apply(&intents),
            Err(PublicAddressError::ForeignProviderState)
        ));
        assert!(!root.join("ownership").exists());
    }

    #[test]
    fn public_realization_cleanup_removes_only_durable_owned_addresses() {
        let root = std::env::temp_dir().join(format!("o3k-public-provider-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("ownership"), PUBLIC_MARKER).expect("ownership");
        let address = Ipv4Addr::new(198, 51, 100, 2);
        store_owned_addresses(&root.join(OWNED_ADDRESS_FILE), &[address]).expect("addresses");
        let command = Arc::new(FakePublicCommand {
            calls: Mutex::new(Vec::new()),
            listing: format!("table ip {PUBLIC_TABLE} {{ comment {PUBLIC_MARKER}; }}"),
            interface_listing: format!("    inet {address}/32 scope global eth0"),
            fail_on: None,
        });
        let mut provider = PublicAddressRealizer::with_command(
            &root,
            "eth0".to_owned(),
            Arc::clone(&command) as Arc<dyn PublicCommand>,
        )
        .expect("provider");

        provider.remove().expect("cleanup");
        let calls = command.calls.lock().expect("calls");
        assert!(
            calls
                .iter()
                .any(|call| { call == &["ip", "addr", "del", "198.51.100.2/32", "dev", "eth0"] })
        );
        assert!(!root.join(OWNED_ADDRESS_FILE).exists());
    }
}
