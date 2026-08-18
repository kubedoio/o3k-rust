//! Durable, project-scoped public IPv4 allocation and association.
//!
//! This is canonical allocation state, not host networking state. A later
//! provider realization consumes the public binding and owns only its
//! node-local uplink address plus DNAT/SNAT mutation.

use o3k_domain::{Ipv4Prefix, NetworkPlanIntent};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Write},
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;
use uuid::Uuid;

const STATE_FILE: &str = "public-addresses.json";
const LOCK_FILE: &str = "public-addresses.lock";
const OWNED_ADDRESS_FILE: &str = "owned-addresses.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicAddressPool {
    pub prefix: Ipv4Prefix,
    pub first_usable: Ipv4Addr,
    pub last_usable: Ipv4Addr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicAddressBinding {
    pub allocation_id: Uuid,
    pub operation_id: String,
    pub project_id: String,
    pub public_address: Ipv4Addr,
    pub endpoint_id: Option<Uuid>,
    pub generation: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    allocations: Vec<PublicAddressBinding>,
}

#[derive(Debug, Error)]
pub enum PublicAddressError {
    #[error("public address pool is invalid")]
    InvalidPool,
    #[error("public address pool is exhausted")]
    Exhausted,
    #[error("public allocation does not exist")]
    NotFound,
    #[error("public allocation is owned by another project")]
    NotOwner,
    #[error("public allocation is already associated with another endpoint")]
    AssociationConflict,
    #[error("public allocation must be disassociated before release")]
    InUse,
    #[error("public allocation state is corrupt")]
    CorruptState,
    #[error("public allocation storage failed: {0}")]
    Storage(#[from] io::Error),
    #[error("public binding has no accepted private endpoint address")]
    MissingEndpoint,
    #[error("public provider found foreign nftables state")]
    ForeignProviderState,
    #[error("public provider command failed")]
    ProviderCommandFailed,
}

pub struct PublicAddressAllocator {
    root: PathBuf,
    pool: PublicAddressPool,
}

impl PublicAddressAllocator {
    pub fn open(
        root: impl Into<PathBuf>,
        pool: PublicAddressPool,
    ) -> Result<Self, PublicAddressError> {
        validate_pool(&pool)?;
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root, pool })
    }

    pub fn allocate(
        &self,
        project_id: &str,
        operation_id: &str,
    ) -> Result<PublicAddressBinding, PublicAddressError> {
        if project_id.trim().is_empty() || operation_id.trim().is_empty() {
            return Err(PublicAddressError::NotFound);
        }
        let _lock = FileLock::acquire(&self.root.join(LOCK_FILE))?;
        let mut state = self.load()?;
        if let Some(existing) = state
            .allocations
            .iter()
            .find(|allocation| allocation.operation_id == operation_id)
        {
            if existing.project_id != project_id {
                return Err(PublicAddressError::NotOwner);
            }
            return Ok(existing.clone());
        }
        let used: std::collections::HashSet<Ipv4Addr> = state
            .allocations
            .iter()
            .map(|allocation| allocation.public_address)
            .collect();
        let Some(public_address) = (u32::from(self.pool.first_usable)
            ..=u32::from(self.pool.last_usable))
            .map(Ipv4Addr::from)
            .find(|address| !used.contains(address))
        else {
            return Err(PublicAddressError::Exhausted);
        };
        let binding = PublicAddressBinding {
            allocation_id: Uuid::now_v7(),
            operation_id: operation_id.to_owned(),
            project_id: project_id.to_owned(),
            public_address,
            endpoint_id: None,
            generation: 1,
        };
        state.allocations.push(binding.clone());
        self.store(&state)?;
        Ok(binding)
    }

    pub fn associate(
        &self,
        project_id: &str,
        allocation_id: Uuid,
        endpoint_id: Uuid,
    ) -> Result<PublicAddressBinding, PublicAddressError> {
        let _lock = FileLock::acquire(&self.root.join(LOCK_FILE))?;
        let mut state = self.load()?;
        let (binding, changed) = {
            let binding = state
                .allocations
                .iter_mut()
                .find(|allocation| allocation.allocation_id == allocation_id)
                .ok_or(PublicAddressError::NotFound)?;
            if binding.project_id != project_id {
                return Err(PublicAddressError::NotOwner);
            }
            if binding
                .endpoint_id
                .is_some_and(|existing| existing != endpoint_id)
            {
                return Err(PublicAddressError::AssociationConflict);
            }
            let changed = binding.endpoint_id != Some(endpoint_id);
            if changed {
                binding.endpoint_id = Some(endpoint_id);
                binding.generation = binding.generation.saturating_add(1);
            }
            (binding.clone(), changed)
        };
        if changed {
            self.store(&state)?;
        }
        Ok(binding)
    }

    pub fn disassociate(
        &self,
        project_id: &str,
        allocation_id: Uuid,
    ) -> Result<PublicAddressBinding, PublicAddressError> {
        let _lock = FileLock::acquire(&self.root.join(LOCK_FILE))?;
        let mut state = self.load()?;
        let (binding, changed) = {
            let binding = state
                .allocations
                .iter_mut()
                .find(|allocation| allocation.allocation_id == allocation_id)
                .ok_or(PublicAddressError::NotFound)?;
            if binding.project_id != project_id {
                return Err(PublicAddressError::NotOwner);
            }
            let changed = binding.endpoint_id.take().is_some();
            if changed {
                binding.generation = binding.generation.saturating_add(1);
            }
            (binding.clone(), changed)
        };
        if changed {
            self.store(&state)?;
        }
        Ok(binding)
    }

    pub fn release(&self, project_id: &str, allocation_id: Uuid) -> Result<(), PublicAddressError> {
        let _lock = FileLock::acquire(&self.root.join(LOCK_FILE))?;
        let mut state = self.load()?;
        let index = state
            .allocations
            .iter()
            .position(|allocation| allocation.allocation_id == allocation_id)
            .ok_or(PublicAddressError::NotFound)?;
        if state.allocations[index].project_id != project_id {
            return Err(PublicAddressError::NotOwner);
        }
        if state.allocations[index].endpoint_id.is_some() {
            return Err(PublicAddressError::InUse);
        }
        state.allocations.remove(index);
        self.store(&state)
    }

    pub fn get(
        &self,
        project_id: &str,
        allocation_id: Uuid,
    ) -> Result<PublicAddressBinding, PublicAddressError> {
        self.load()?
            .allocations
            .into_iter()
            .find(|allocation| {
                allocation.allocation_id == allocation_id && allocation.project_id == project_id
            })
            .ok_or(PublicAddressError::NotFound)
    }

    fn load(&self) -> Result<State, PublicAddressError> {
        match fs::read(self.root.join(STATE_FILE)) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| PublicAddressError::CorruptState)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(State::default()),
            Err(error) => Err(error.into()),
        }
    }

    fn store(&self, state: &State) -> Result<(), PublicAddressError> {
        let bytes =
            serde_json::to_vec_pretty(state).map_err(|_| PublicAddressError::CorruptState)?;
        let path = self.root.join(STATE_FILE);
        let temporary = path.with_extension("json.tmp");
        let mut file = File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(temporary, path)?;
        Ok(())
    }
}

fn validate_pool(pool: &PublicAddressPool) -> Result<(), PublicAddressError> {
    if !pool.prefix.contains(pool.first_usable)
        || !pool.prefix.contains(pool.last_usable)
        || pool.first_usable > pool.last_usable
        || pool.first_usable == pool.prefix.network
        || pool.last_usable
            == Ipv4Addr::from(u32::from(pool.prefix.network) + (!0u32 >> pool.prefix.prefix_len))
    {
        return Err(PublicAddressError::InvalidPool);
    }
    Ok(())
}

struct FileLock {
    path: PathBuf,
}

impl FileLock {
    fn acquire(path: &Path) -> Result<Self, PublicAddressError> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(_) => {
                    return Ok(Self {
                        path: path.to_owned(),
                    });
                }
                Err(error)
                    if error.kind() == ErrorKind::AlreadyExists && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Node-local public-address and DNAT/SNAT realization for already-authorized
/// public bindings. The allocator remains the control-plane authority; this
/// provider owns only exact recorded uplink addresses and its marked nftables
/// table and cannot choose a different address.
pub struct PublicAddressRealizer {
    root: PathBuf,
    uplink: String,
    command: Arc<dyn PublicCommand>,
    owned: bool,
    owned_addresses: Vec<Ipv4Addr>,
}

trait PublicCommand: Send + Sync {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)>;
    fn run(&self, program: &str, args: &[&str]) -> io::Result<bool>;
}

struct SystemPublicCommand;

impl PublicCommand for SystemPublicCommand {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)> {
        let output = std::process::Command::new(program).args(args).output()?;
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }

    fn run(&self, program: &str, args: &[&str]) -> io::Result<bool> {
        Ok(std::process::Command::new(program)
            .args(args)
            .status()?
            .success())
    }
}

const PUBLIC_TABLE: &str = "o3k_public";
const PUBLIC_MARKER: &str = "o3k-p9-public";

impl PublicAddressRealizer {
    pub fn open(root: impl Into<PathBuf>, uplink: String) -> Result<Self, PublicAddressError> {
        if uplink.is_empty() || uplink.len() > 15 {
            return Err(PublicAddressError::InvalidPool);
        }
        let root = root.into();
        fs::create_dir_all(&root)?;
        let owned = root.join("ownership").exists();
        let owned_addresses = load_owned_addresses(&root.join(OWNED_ADDRESS_FILE))?;
        Ok(Self {
            root,
            uplink,
            command: Arc::new(SystemPublicCommand),
            owned,
            owned_addresses,
        })
    }

    #[cfg(test)]
    fn with_command(
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
        let bindings: Vec<(&Uuid, &Ipv4Addr)> = intents
            .iter()
            .filter_map(|intent| match intent {
                NetworkPlanIntent::PublicAddressBinding(binding) => {
                    Some((&binding.endpoint_id, &binding.public_address))
                }
                _ => None,
            })
            .collect();
        if bindings.is_empty() {
            return Ok(());
        }
        for (endpoint_id, _) in &bindings {
            if !endpoint_addresses.contains_key(endpoint_id) {
                return Err(PublicAddressError::MissingEndpoint);
            }
        }
        let table_exists = self.ensure_foreign_safe()?;
        if table_exists && !self.owned {
            return Err(PublicAddressError::ForeignProviderState);
        }
        let mut desired_addresses: Vec<Ipv4Addr> =
            bindings.iter().map(|(_, address)| **address).collect();
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
        // Establish the provider ownership marker before the first host
        // mutation so a crash during address or nft realization remains
        // discoverable by restart cleanup.
        fs::write(self.root.join("ownership"), PUBLIC_MARKER)?;
        self.owned = true;
        // Record accepted ownership before adding addresses. An interrupted
        // add remains recoverable as owned state and is verified again during
        // cleanup before any delete is issued.
        store_owned_addresses(&self.root.join(OWNED_ADDRESS_FILE), &desired_addresses)?;
        self.owned_addresses = desired_addresses.clone();
        // Replays rebuild only the provider's own marked table. This avoids
        // duplicate DNAT/SNAT rules after reconnect while never touching a
        // table that failed the ownership marker check above.
        if table_exists
            && !self
                .command
                .run("nft", &["delete", "table", "ip", PUBLIC_TABLE])
                .map_err(PublicAddressError::Storage)?
        {
            return Err(PublicAddressError::ProviderCommandFailed);
        }
        let table_args = [
            "add",
            "table",
            "ip",
            PUBLIC_TABLE,
            "{",
            "comment",
            PUBLIC_MARKER,
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
        let _ = self.command.run("nft", &prerouting);
        let _ = self.command.run("nft", &postrouting);
        for (endpoint_id, public_address) in bindings {
            let private_address = endpoint_addresses[endpoint_id].to_string();
            let public_address = public_address.to_string();
            let uplink = format!("\"{}\"", self.uplink);
            let comment = format!("{}:{}", PUBLIC_MARKER, endpoint_id);
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
        self.owned = false;
        self.owned_addresses.clear();
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

fn load_owned_addresses(path: &Path) -> Result<Vec<Ipv4Addr>, PublicAddressError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| PublicAddressError::CorruptState),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn store_owned_addresses(path: &Path, addresses: &[Ipv4Addr]) -> Result<(), PublicAddressError> {
    let bytes =
        serde_json::to_vec_pretty(addresses).map_err(|_| PublicAddressError::CorruptState)?;
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
    use std::sync::Mutex;

    struct FakePublicCommand {
        calls: Mutex<Vec<Vec<String>>>,
        listing: String,
        interface_listing: String,
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
            self.calls.lock().expect("calls").push(
                std::iter::once(program.to_owned())
                    .chain(args.iter().map(|arg| (*arg).to_owned()))
                    .collect(),
            );
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
        vec![NetworkPlanIntent::PublicAddressBinding(
            o3k_domain::PublicAddressBindingIntent {
                id: Uuid::from_u128(3),
                project_id: "project-a".to_owned(),
                public_address: Ipv4Addr::new(198, 51, 100, 2),
                endpoint_id,
                generation: 1,
            },
        )]
    }

    #[test]
    fn public_realization_rejects_missing_endpoint_before_mutation() {
        let root = std::env::temp_dir().join(format!("o3k-public-provider-{}", Uuid::now_v7()));
        let command = Arc::new(FakePublicCommand {
            calls: Mutex::new(Vec::new()),
            listing: String::new(),
            interface_listing: String::new(),
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
    fn public_realization_never_adopts_foreign_table() {
        let root = std::env::temp_dir().join(format!("o3k-public-provider-{}", Uuid::now_v7()));
        let command = Arc::new(FakePublicCommand {
            calls: Mutex::new(Vec::new()),
            listing: "table ip o3k_public { comment foreign; }".to_owned(),
            interface_listing: String::new(),
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
            NetworkPlanIntent::AddressAssignment {
                endpoint_id: endpoint,
                address: Ipv4Addr::new(10, 0, 0, 2),
                generation: 1,
            },
            intents[0].clone(),
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
                PUBLIC_MARKER,
                "}"
            ]));
        assert!(
            calls
                .iter()
                .any(|call| { call == &["ip", "addr", "add", "198.51.100.2/32", "dev", "eth0"] })
        );
    }

    #[test]
    fn public_realization_never_adopts_foreign_uplink_address() {
        let root = std::env::temp_dir().join(format!("o3k-public-provider-{}", Uuid::now_v7()));
        let command = Arc::new(FakePublicCommand {
            calls: Mutex::new(Vec::new()),
            listing: String::new(),
            interface_listing: "    inet 198.51.100.2/32 scope global eth0".to_owned(),
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
