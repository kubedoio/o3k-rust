//! Durable bounded console output for O3K-managed instances.

use std::{fs, io, path::PathBuf};
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
}

impl ConsoleService {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ConsoleError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(ConsoleError::Storage)?;
        Ok(Self {
            root,
            max_bytes: MAX_CONSOLE_BYTES,
        })
    }

    pub fn write(&self, instance_id: Uuid, output: &[u8]) -> Result<(), ConsoleError> {
        if output.len() > self.max_bytes {
            return Err(ConsoleError::InvalidInput);
        }
        let path = self.path(instance_id)?;
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, output).map_err(ConsoleError::Storage)?;
        fs::rename(temporary, path).map_err(ConsoleError::Storage)
    }

    pub fn append(&self, instance_id: Uuid, output: &[u8]) -> Result<(), ConsoleError> {
        let mut current = self.read(instance_id).unwrap_or_default();
        current.extend_from_slice(output);
        if current.len() > self.max_bytes {
            current = current[current.len() - self.max_bytes..].to_vec();
        }
        self.write(instance_id, &current)
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

    pub fn cleanup(&self, instance_id: Uuid) -> Result<(), ConsoleError> {
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
        let restarted = ConsoleService::open(service.root.clone())?;
        assert_eq!(restarted.read(id)?.len(), MAX_CONSOLE_BYTES);
        restarted.cleanup(id)?;
        restarted.cleanup(id)?;
        assert!(matches!(restarted.read(id), Err(ConsoleError::NotFound)));
        Ok(())
    }
}
