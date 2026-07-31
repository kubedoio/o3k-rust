//! Durable bounded console output for O3K-managed instances.

use std::{
    collections::HashMap,
    fs, io,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use thiserror::Error;
use uuid::Uuid;

pub const MAX_CONSOLE_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum ConsoleError {
    #[error("console output was not found")]
    NotFound,
    #[error("console output storage failed")]
    Storage(#[source] io::Error),
    #[error("console output is invalid")]
    InvalidInput,
}

#[derive(Clone)]
pub struct ConsoleService {
    root: PathBuf,
    max_bytes: usize,
    locks: Arc<Mutex<HashMap<Uuid, Arc<Mutex<()>>>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleChunk {
    pub bytes: Vec<u8>,
    pub offset: u64,
    pub next_offset: u64,
    pub truncated: bool,
}

impl ConsoleService {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ConsoleError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(ConsoleError::Storage)?;
        Ok(Self {
            root,
            max_bytes: MAX_CONSOLE_BYTES,
            locks: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn write(&self, instance_id: Uuid, output: &[u8]) -> Result<(), ConsoleError> {
        if output.len() > self.max_bytes {
            return Err(ConsoleError::InvalidInput);
        }
        let lock = self.instance_lock(instance_id)?;
        let _guard = lock.lock().map_err(|_| ConsoleError::InvalidInput)?;
        self.write_unlocked(instance_id, output)
    }

    fn write_unlocked(&self, instance_id: Uuid, output: &[u8]) -> Result<(), ConsoleError> {
        let path = self.path(instance_id)?;
        let temporary = path.with_extension(format!("tmp-{}", Uuid::now_v7()));
        if let Err(error) = fs::write(&temporary, output) {
            let _ = fs::remove_file(&temporary);
            return Err(ConsoleError::Storage(error));
        }
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(ConsoleError::Storage(error));
        }
        Ok(())
    }

    pub fn append(&self, instance_id: Uuid, output: &[u8]) -> Result<(), ConsoleError> {
        let lock = self.instance_lock(instance_id)?;
        let _guard = lock.lock().map_err(|_| ConsoleError::InvalidInput)?;
        self.append_unlocked(instance_id, output)
    }

    fn append_unlocked(&self, instance_id: Uuid, output: &[u8]) -> Result<(), ConsoleError> {
        let mut current = self.read(instance_id).unwrap_or_default();
        current.extend_from_slice(output);
        if current.len() > self.max_bytes {
            current = current[current.len() - self.max_bytes..].to_vec();
        }
        self.write_unlocked(instance_id, &current)
    }

    /// Persists a sequential agent observation without allowing stale or
    /// out-of-order chunks to corrupt the durable console buffer.
    pub fn write_chunk(
        &self,
        instance_id: Uuid,
        offset: u64,
        output: &[u8],
    ) -> Result<(), ConsoleError> {
        let offset = usize::try_from(offset).map_err(|_| ConsoleError::InvalidInput)?;
        let lock = self.instance_lock(instance_id)?;
        let _guard = lock.lock().map_err(|_| ConsoleError::InvalidInput)?;
        let current = self.read(instance_id).unwrap_or_default();
        if offset == 0 {
            if current.is_empty() {
                return self.write_unlocked(instance_id, output);
            }
            if current == output {
                return Ok(());
            }
            if output.starts_with(&current) {
                return self.write_unlocked(instance_id, output);
            }
            return Err(ConsoleError::InvalidInput);
        }
        if offset > current.len() || output.len() > self.max_bytes.saturating_sub(offset) {
            return Err(ConsoleError::InvalidInput);
        }
        if current.len() >= offset.saturating_add(output.len())
            && current[offset..offset + output.len()] == *output
        {
            return Ok(());
        }
        if offset != current.len() {
            return Err(ConsoleError::InvalidInput);
        }
        self.append_unlocked(instance_id, output)
    }

    pub fn read(&self, instance_id: Uuid) -> Result<Vec<u8>, ConsoleError> {
        fs::read(self.path(instance_id)?).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                ConsoleError::NotFound
            } else {
                ConsoleError::Storage(error)
            }
        })
    }

    pub fn read_from(
        &self,
        instance_id: Uuid,
        offset: u64,
        max_bytes: usize,
    ) -> Result<ConsoleChunk, ConsoleError> {
        let bytes = self.read(instance_id)?;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let end = start
            .saturating_add(max_bytes.min(self.max_bytes))
            .min(bytes.len());
        Ok(ConsoleChunk {
            bytes: bytes[start..end].to_vec(),
            offset: start as u64,
            next_offset: end as u64,
            truncated: end < bytes.len(),
        })
    }

    pub fn cleanup(&self, instance_id: Uuid) -> Result<(), ConsoleError> {
        let lock = self.instance_lock(instance_id)?;
        let _guard = lock.lock().map_err(|_| ConsoleError::InvalidInput)?;
        let path = self.path(instance_id)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ConsoleError::Storage(error)),
        }
    }

    pub fn path(&self, instance_id: Uuid) -> Result<PathBuf, ConsoleError> {
        if instance_id == Uuid::nil() {
            return Err(ConsoleError::InvalidInput);
        }
        Ok(self.root.join(format!("{instance_id}.log")))
    }

    fn instance_lock(&self, instance_id: Uuid) -> Result<Arc<Mutex<()>>, ConsoleError> {
        if instance_id == Uuid::nil() {
            return Err(ConsoleError::InvalidInput);
        }
        let mut locks = self.locks.lock().map_err(|_| ConsoleError::InvalidInput)?;
        Ok(locks
            .entry(instance_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn service() -> Result<ConsoleService, ConsoleError> {
        ConsoleService::open(std::env::temp_dir().join(format!("o3k-console-{}", Uuid::now_v7())))
    }
    #[test]
    fn output_is_bounded_durable_and_cleanup_is_idempotent() -> Result<(), ConsoleError> {
        let service = service()?;
        let id = Uuid::now_v7();
        service.append(id, b"boot\n")?;
        service.append(id, &vec![b'x'; MAX_CONSOLE_BYTES + 10])?;
        assert_eq!(service.read(id)?.len(), MAX_CONSOLE_BYTES);
        let chunk = service.read_from(id, 10, 20)?;
        assert_eq!(chunk.offset, 10);
        assert_eq!(chunk.next_offset, 30);
        assert!(chunk.truncated);
        let restarted = ConsoleService::open(service.root.clone())?;
        assert_eq!(restarted.read(id)?.len(), MAX_CONSOLE_BYTES);
        restarted.cleanup(id)?;
        restarted.cleanup(id)?;
        assert!(matches!(restarted.read(id), Err(ConsoleError::NotFound)));
        Ok(())
    }

    #[test]
    fn observation_chunks_are_sequential_and_replay_safe() -> Result<(), ConsoleError> {
        let service = service()?;
        let id = Uuid::now_v7();
        service.write_chunk(id, 0, b"boot ")?;
        service.write_chunk(id, 0, b"boot ")?;
        service.write_chunk(id, 0, b"boot output\n")?;
        service.write_chunk(id, 5, b"output\n")?;
        service.write_chunk(id, 5, b"output\n")?;
        assert_eq!(service.read(id)?, b"boot output\n");
        assert!(matches!(
            service.write_chunk(id, 0, b"stale"),
            Err(ConsoleError::InvalidInput)
        ));
        assert!(matches!(
            service.write_chunk(id, 2, b"stale"),
            Err(ConsoleError::InvalidInput)
        ));
        service.cleanup(id)?;
        Ok(())
    }

    #[test]
    fn concurrent_appends_are_serialized() -> Result<(), ConsoleError> {
        let service = service()?;
        let id = Uuid::now_v7();
        let left = service.clone();
        let right = service.clone();
        let first = std::thread::spawn(move || left.append(id, b"left"));
        let second = std::thread::spawn(move || right.append(id, b"right"));
        first.join().map_err(|_| ConsoleError::InvalidInput)??;
        second.join().map_err(|_| ConsoleError::InvalidInput)??;
        let output = service.read(id)?;
        assert!(output == b"leftright" || output == b"rightleft");
        Ok(())
    }
}
