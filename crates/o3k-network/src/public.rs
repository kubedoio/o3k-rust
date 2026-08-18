//! Durable, project-scoped public IPv4 allocation and association.
//!
//! This is canonical allocation state, not nftables state. A later provider
//! realization consumes [`PublicAddressBinding`] and owns only its node-local
//! DNAT/SNAT mutation.

use o3k_domain::Ipv4Prefix;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Write},
    net::Ipv4Addr,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;
use uuid::Uuid;

const STATE_FILE: &str = "public-addresses.json";
const LOCK_FILE: &str = "public-addresses.lock";

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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

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
}
