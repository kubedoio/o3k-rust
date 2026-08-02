use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use o3k_provider_contract::compute_proto as proto;
use prost::Message;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ARTIFACT_CHUNK_BYTES: usize = 256 * 1024;
const MAGIC: &[u8] = b"O3KART1";

#[derive(Debug, Error)]
pub enum ArtifactStoreError {
    #[error("artifact offer is invalid")]
    InvalidOffer,
    #[error("artifact transfer conflicts with existing state")]
    Conflict,
    #[error("artifact chunk is invalid")]
    InvalidChunk,
    #[error("artifact digest or size does not match")]
    DigestMismatch,
    #[error("artifact storage failed")]
    Storage(#[source] io::Error),
    #[error("artifact manifest is corrupt")]
    CorruptManifest,
    #[error("artifact path is not owned")]
    UnownedPath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactReceipt {
    pub transfer_id: String,
    pub next_chunk_index: u32,
    pub contiguous_bytes: u64,
    pub state: proto::ArtifactTransferState,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
    agent_id: String,
}

#[derive(Debug, Clone)]
struct Manifest {
    offer: proto::ArtifactOffer,
    state: i32,
    next_chunk: u32,
    bytes: u64,
}

impl ArtifactStore {
    pub fn open(
        root: impl Into<PathBuf>,
        agent_id: impl Into<String>,
    ) -> Result<Self, ArtifactStoreError> {
        let root = root.into();
        let agent_id = agent_id.into();
        if !valid_reference(&agent_id) {
            return Err(ArtifactStoreError::InvalidOffer);
        }
        match fs::symlink_metadata(&root) {
            Ok(meta) if meta.file_type().is_dir() => {}
            Ok(_) => return Err(ArtifactStoreError::UnownedPath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(&root).map_err(ArtifactStoreError::Storage)?;
            }
            Err(error) => return Err(ArtifactStoreError::Storage(error)),
        }
        Ok(Self { root, agent_id })
    }

    pub fn begin(
        &self,
        offer: &proto::ArtifactOffer,
    ) -> Result<ArtifactReceipt, ArtifactStoreError> {
        validate_offer(offer, &self.agent_id)?;
        let manifest_path = self.manifest_path(&offer.transfer_id)?;
        if manifest_path.exists() || manifest_path.is_symlink() {
            if manifest_path.is_symlink() {
                return Err(ArtifactStoreError::UnownedPath);
            }
            let manifest = read_manifest(&manifest_path)?;
            same_offer(&manifest.offer, offer)?;
            if manifest.state == proto::ArtifactTransferState::Committed as i32 {
                let path = self.final_path(offer)?;
                verify_file(&path, offer)?;
                return Ok(receipt(&manifest, Some(path)));
            }
            let part = self.part_path(&offer.transfer_id)?;
            if part.is_symlink()
                || fs::metadata(&part)
                    .map_err(ArtifactStoreError::Storage)?
                    .len()
                    != manifest.bytes
            {
                return Err(ArtifactStoreError::CorruptManifest);
            }
            return Ok(receipt(&manifest, None));
        }
        let manifest = Manifest {
            offer: offer.clone(),
            state: proto::ArtifactTransferState::Offered as i32,
            next_chunk: 0,
            bytes: 0,
        };
        atomic_manifest(&manifest_path, &manifest)?;
        let mut options = OpenOptions::new();
        options.create_new(true).write(true).private();
        options
            .open(self.part_path(&offer.transfer_id)?)
            .map_err(ArtifactStoreError::Storage)?
            .sync_all()
            .map_err(ArtifactStoreError::Storage)?;
        Ok(receipt(&manifest, None))
    }

    pub fn accept_chunk(
        &self,
        offer: &proto::ArtifactOffer,
        chunk: &proto::ArtifactChunk,
    ) -> Result<ArtifactReceipt, ArtifactStoreError> {
        validate_offer(offer, &self.agent_id)?;
        if chunk.transfer_id != offer.transfer_id
            || chunk.data.is_empty()
            || chunk.data.len() > MAX_ARTIFACT_CHUNK_BYTES
            || !valid_sha256(&chunk.chunk_sha256)
            || digest(&chunk.data) != chunk.chunk_sha256
        {
            return Err(ArtifactStoreError::InvalidChunk);
        }
        let manifest_path = self.manifest_path(&offer.transfer_id)?;
        let mut manifest = read_manifest(&manifest_path)?;
        same_offer(&manifest.offer, offer)?;
        let expected_offset = u64::from(chunk.chunk_index) * u64::from(offer.chunk_size_bytes);
        if manifest.state == proto::ArtifactTransferState::Committed as i32 {
            if chunk.chunk_index >= manifest.next_chunk || chunk.offset_bytes != expected_offset {
                return Err(ArtifactStoreError::Conflict);
            }
            let mut file =
                File::open(self.final_path(offer)?).map_err(ArtifactStoreError::Storage)?;
            file.seek(SeekFrom::Start(expected_offset))
                .map_err(ArtifactStoreError::Storage)?;
            let mut existing = vec![0; chunk.data.len()];
            file.read_exact(&mut existing)
                .map_err(|_| ArtifactStoreError::Conflict)?;
            return if existing == chunk.data {
                Ok(receipt(&manifest, Some(self.final_path(offer)?)))
            } else {
                Err(ArtifactStoreError::Conflict)
            };
        }
        if chunk.chunk_index < manifest.next_chunk {
            if chunk.offset_bytes != expected_offset {
                return Err(ArtifactStoreError::Conflict);
            }
            let mut file = File::open(self.part_path(&offer.transfer_id)?)
                .map_err(ArtifactStoreError::Storage)?;
            file.seek(SeekFrom::Start(expected_offset))
                .map_err(ArtifactStoreError::Storage)?;
            let mut existing = vec![0; chunk.data.len()];
            file.read_exact(&mut existing)
                .map_err(|_| ArtifactStoreError::Conflict)?;
            if existing != chunk.data {
                return Err(ArtifactStoreError::Conflict);
            }
            return Ok(receipt(&manifest, None));
        }
        if chunk.chunk_index != manifest.next_chunk
            || chunk.offset_bytes != manifest.bytes
            || chunk.offset_bytes != expected_offset
        {
            return Err(ArtifactStoreError::InvalidChunk);
        }
        let end = manifest
            .bytes
            .checked_add(chunk.data.len() as u64)
            .ok_or(ArtifactStoreError::InvalidChunk)?;
        if end > offer.size_bytes
            || (chunk.data.len() < offer.chunk_size_bytes as usize && end != offer.size_bytes)
        {
            return Err(ArtifactStoreError::InvalidChunk);
        }
        let mut file = OpenOptions::new()
            .append(true)
            .open(self.part_path(&offer.transfer_id)?)
            .map_err(ArtifactStoreError::Storage)?;
        file.write_all(&chunk.data)
            .map_err(ArtifactStoreError::Storage)?;
        file.sync_data().map_err(ArtifactStoreError::Storage)?;
        manifest.state = proto::ArtifactTransferState::Receiving as i32;
        manifest.next_chunk = manifest
            .next_chunk
            .checked_add(1)
            .ok_or(ArtifactStoreError::InvalidChunk)?;
        manifest.bytes = end;
        atomic_manifest(&manifest_path, &manifest)?;
        Ok(receipt(&manifest, None))
    }

    pub fn finish(
        &self,
        offer: &proto::ArtifactOffer,
        end: &proto::ArtifactEnd,
    ) -> Result<ArtifactReceipt, ArtifactStoreError> {
        validate_offer(offer, &self.agent_id)?;
        if end.transfer_id != offer.transfer_id
            || end.sha256 != offer.sha256
            || end.size_bytes != offer.size_bytes
        {
            return Err(ArtifactStoreError::DigestMismatch);
        }
        let manifest_path = self.manifest_path(&offer.transfer_id)?;
        let mut manifest = read_manifest(&manifest_path)?;
        same_offer(&manifest.offer, offer)?;
        if manifest.state == proto::ArtifactTransferState::Committed as i32 {
            let path = self.final_path(offer)?;
            verify_file(&path, offer)?;
            return Ok(receipt(&manifest, Some(path)));
        }
        if manifest.bytes != offer.size_bytes || manifest.next_chunk != offer.chunk_count {
            return Err(ArtifactStoreError::DigestMismatch);
        }
        let part = self.part_path(&offer.transfer_id)?;
        verify_file(&part, offer)?;
        let final_path = self.final_path(offer)?;
        if final_path.exists() || final_path.is_symlink() {
            if final_path.is_symlink() {
                return Err(ArtifactStoreError::UnownedPath);
            }
            verify_file(&final_path, offer)?;
            fs::remove_file(&part).map_err(ArtifactStoreError::Storage)?;
        } else {
            fs::rename(&part, &final_path).map_err(ArtifactStoreError::Storage)?;
        }
        manifest.state = proto::ArtifactTransferState::Committed as i32;
        atomic_manifest(&manifest_path, &manifest)?;
        Ok(receipt(&manifest, Some(final_path)))
    }

    pub fn resolve(&self, offer: &proto::ArtifactOffer) -> Result<PathBuf, ArtifactStoreError> {
        validate_offer(offer, &self.agent_id)?;
        let manifest = read_manifest(&self.manifest_path(&offer.transfer_id)?)?;
        same_offer(&manifest.offer, offer)?;
        if manifest.state != proto::ArtifactTransferState::Committed as i32 {
            return Err(ArtifactStoreError::Conflict);
        }
        let path = self.final_path(offer)?;
        verify_file(&path, offer)?;
        Ok(path)
    }

    fn manifest_path(&self, id: &str) -> Result<PathBuf, ArtifactStoreError> {
        valid_reference(id)
            .then(|| self.root.join(format!(".{id}.manifest")))
            .ok_or(ArtifactStoreError::InvalidOffer)
    }
    fn part_path(&self, id: &str) -> Result<PathBuf, ArtifactStoreError> {
        valid_reference(id)
            .then(|| self.root.join(format!(".{id}.part")))
            .ok_or(ArtifactStoreError::InvalidOffer)
    }
    fn final_path(&self, offer: &proto::ArtifactOffer) -> Result<PathBuf, ArtifactStoreError> {
        if !valid_sha256(&offer.sha256) || !valid_reference(&offer.format) {
            return Err(ArtifactStoreError::InvalidOffer);
        }
        Ok(self.root.join(format!("{}.{}", offer.sha256, offer.format)))
    }
}

fn receipt(manifest: &Manifest, path: Option<PathBuf>) -> ArtifactReceipt {
    ArtifactReceipt {
        transfer_id: manifest.offer.transfer_id.clone(),
        next_chunk_index: manifest.next_chunk,
        contiguous_bytes: manifest.bytes,
        state: proto::ArtifactTransferState::try_from(manifest.state)
            .unwrap_or(proto::ArtifactTransferState::Rejected),
        path,
    }
}

fn validate_offer(offer: &proto::ArtifactOffer, agent: &str) -> Result<(), ArtifactStoreError> {
    let chunks = offer
        .size_bytes
        .div_ceil(u64::from(offer.chunk_size_bytes.max(1)));
    if !valid_reference(&offer.transfer_id)
        || !valid_reference(&offer.command_id)
        || !valid_reference(&offer.operation_id)
        || !valid_reference(&offer.resource_id)
        || !valid_reference(&offer.artifact_id)
        || offer.agent_id != agent
        || !valid_sha256(&offer.sha256)
        || offer.size_bytes == 0
        || offer.size_bytes > MAX_ARTIFACT_BYTES
        || offer.chunk_size_bytes == 0
        || offer.chunk_size_bytes as usize > MAX_ARTIFACT_CHUNK_BYTES
        || offer.chunk_count == 0
        || u64::from(offer.chunk_count) != chunks
        || !matches!(offer.kind, 1 | 2)
        || !matches!(offer.format.as_str(), "raw" | "qcow2" | "iso")
        || (offer.kind == proto::ArtifactKind::ImageBase as i32 && offer.format == "iso")
        || (offer.kind == proto::ArtifactKind::ConfigDriveIso as i32 && offer.format != "iso")
    {
        return Err(ArtifactStoreError::InvalidOffer);
    }
    Ok(())
}

fn same_offer(
    left: &proto::ArtifactOffer,
    right: &proto::ArtifactOffer,
) -> Result<(), ArtifactStoreError> {
    (left == right)
        .then_some(())
        .ok_or(ArtifactStoreError::Conflict)
}

fn verify_file(path: &Path, offer: &proto::ArtifactOffer) -> Result<(), ArtifactStoreError> {
    let meta = fs::symlink_metadata(path).map_err(ArtifactStoreError::Storage)?;
    if !meta.file_type().is_file() || meta.len() != offer.size_bytes {
        return Err(ArtifactStoreError::DigestMismatch);
    }
    let mut file = File::open(path).map_err(ArtifactStoreError::Storage)?;
    let mut hasher = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(ArtifactStoreError::Storage)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        hasher.update(&buffer[..count]);
    }
    if bytes != offer.size_bytes || digest_hasher(hasher.finalize()) != offer.sha256 {
        return Err(ArtifactStoreError::DigestMismatch);
    }
    Ok(())
}

fn atomic_manifest(path: &Path, manifest: &Manifest) -> Result<(), ArtifactStoreError> {
    let offer = manifest.offer.encode_to_vec();
    let mut bytes = Vec::with_capacity(MAGIC.len() + 1 + 4 + offer.len() + 16);
    bytes.extend_from_slice(MAGIC);
    bytes.push(1);
    bytes.extend_from_slice(&(offer.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&offer);
    bytes.extend_from_slice(&manifest.state.to_le_bytes());
    bytes.extend_from_slice(&manifest.next_chunk.to_le_bytes());
    bytes.extend_from_slice(&manifest.bytes.to_le_bytes());
    let temp = path.with_extension("manifest.tmp");
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true).private();
    let mut file = options.open(&temp).map_err(ArtifactStoreError::Storage)?;
    file.write_all(&bytes)
        .map_err(ArtifactStoreError::Storage)?;
    file.sync_all().map_err(ArtifactStoreError::Storage)?;
    fs::rename(temp, path).map_err(ArtifactStoreError::Storage)
}

fn read_manifest(path: &Path) -> Result<Manifest, ArtifactStoreError> {
    let bytes = fs::read(path).map_err(ArtifactStoreError::Storage)?;
    if bytes.len() < MAGIC.len() + 1 + 4 + 16
        || !bytes.starts_with(MAGIC)
        || bytes[MAGIC.len()] != 1
    {
        return Err(ArtifactStoreError::CorruptManifest);
    }
    let mut cursor = MAGIC.len() + 1;
    let length = u32::from_le_bytes(
        bytes[cursor..cursor + 4]
            .try_into()
            .map_err(|_| ArtifactStoreError::CorruptManifest)?,
    ) as usize;
    cursor += 4;
    if cursor
        .checked_add(length)
        .and_then(|value| value.checked_add(16))
        != Some(bytes.len())
    {
        return Err(ArtifactStoreError::CorruptManifest);
    }
    let offer = proto::ArtifactOffer::decode(&bytes[cursor..cursor + length])
        .map_err(|_| ArtifactStoreError::CorruptManifest)?;
    cursor += length;
    let state = i32::from_le_bytes(
        bytes[cursor..cursor + 4]
            .try_into()
            .map_err(|_| ArtifactStoreError::CorruptManifest)?,
    );
    cursor += 4;
    let next_chunk = u32::from_le_bytes(
        bytes[cursor..cursor + 4]
            .try_into()
            .map_err(|_| ArtifactStoreError::CorruptManifest)?,
    );
    cursor += 4;
    let amount = u64::from_le_bytes(
        bytes[cursor..cursor + 8]
            .try_into()
            .map_err(|_| ArtifactStoreError::CorruptManifest)?,
    );
    Ok(Manifest {
        offer,
        state,
        next_chunk,
        bytes: amount,
    })
}

fn valid_reference(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.starts_with('.')
        && value
            == Path::new(value)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
}
fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|value| value.is_ascii_hexdigit())
}
fn digest(bytes: &[u8]) -> String {
    digest_hasher(Sha256::digest(bytes))
}
fn digest_hasher<D: AsRef<[u8]>>(value: D) -> String {
    use std::fmt::Write as _;
    let mut result = String::with_capacity(64);
    for byte in value.as_ref() {
        let _ = write!(&mut result, "{byte:02x}");
    }
    result
}

trait PrivateOpenOptions {
    fn private(&mut self);
}
impl PrivateOpenOptions for OpenOptions {
    fn private(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            self.mode(0o600);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn fixture(root: &Path) -> (ArtifactStore, proto::ArtifactOffer, Vec<u8>) {
        let content = b"abcdefgh".to_vec();
        let offer = proto::ArtifactOffer {
            transfer_id: Uuid::now_v7().to_string(),
            command_id: "command-1".into(),
            operation_id: "operation-1".into(),
            resource_id: "resource-1".into(),
            agent_id: "agent-1".into(),
            artifact_id: "image-1".into(),
            kind: proto::ArtifactKind::ImageBase as i32,
            sha256: digest(&content),
            size_bytes: content.len() as u64,
            format: "raw".into(),
            chunk_size_bytes: 4,
            chunk_count: 2,
            expires_at_unix_ms: i64::MAX,
        };
        (
            ArtifactStore::open(root, "agent-1").unwrap(),
            offer,
            content,
        )
    }

    #[test]
    fn commits_and_reopens() {
        let root = std::env::temp_dir().join(format!("o3k-artifact-{}", Uuid::now_v7()));
        let (store, offer, content) = fixture(&root);
        store.begin(&offer).unwrap();
        for (index, data) in content.chunks(4).enumerate() {
            store
                .accept_chunk(
                    &offer,
                    &proto::ArtifactChunk {
                        transfer_id: offer.transfer_id.clone(),
                        chunk_index: index as u32,
                        offset_bytes: (index * 4) as u64,
                        data: data.to_vec(),
                        chunk_sha256: digest(data),
                    },
                )
                .unwrap();
        }
        let receipt = store
            .finish(
                &offer,
                &proto::ArtifactEnd {
                    transfer_id: offer.transfer_id.clone(),
                    sha256: offer.sha256.clone(),
                    size_bytes: offer.size_bytes,
                },
            )
            .unwrap();
        assert_eq!(fs::read(receipt.path.unwrap()).unwrap(), content);
        let reopened = ArtifactStore::open(&root, "agent-1").unwrap();
        assert!(reopened.resolve(&offer).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_chunk_is_idempotent_but_conflict_is_rejected() {
        let root = std::env::temp_dir().join(format!("o3k-artifact-{}", Uuid::now_v7()));
        let (store, offer, content) = fixture(&root);
        store.begin(&offer).unwrap();
        let chunk = proto::ArtifactChunk {
            transfer_id: offer.transfer_id.clone(),
            chunk_index: 0,
            offset_bytes: 0,
            data: content[..4].to_vec(),
            chunk_sha256: digest(&content[..4]),
        };
        store.accept_chunk(&offer, &chunk).unwrap();
        assert!(store.accept_chunk(&offer, &chunk).is_ok());
        let mut conflict = chunk;
        conflict.data = b"xxxx".to_vec();
        conflict.chunk_sha256 = digest(&conflict.data);
        assert!(matches!(
            store.accept_chunk(&offer, &conflict),
            Err(ArtifactStoreError::Conflict)
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
