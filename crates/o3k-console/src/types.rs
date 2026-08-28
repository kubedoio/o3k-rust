//! Console domain types: service, chunk, errors.

use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use thiserror::Error;
use uuid::Uuid;

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
    pub(crate) root: PathBuf,
    pub(crate) max_bytes: usize,
    pub(crate) locks: Arc<Mutex<HashMap<Uuid, Arc<Mutex<()>>>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleChunk {
    pub bytes: Vec<u8>,
    pub offset: u64,
    pub next_offset: u64,
    pub truncated: bool,
}
