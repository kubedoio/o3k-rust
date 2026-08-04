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
pub const MAX_ARTIFACT_CHUNKS: u32 = 256;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactCleanup {
    AlreadyAbsent,
    Removed,
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

/// Complete durable identity required to resolve an agent-local image base.
/// The type is crate-internal so the returned path cannot become a wire or
/// OpenStack API value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommittedImageQuery {
    pub transfer_id: String,
    pub command_id: String,
    pub operation_id: String,
    pub resource_id: String,
    pub agent_id: String,
    pub artifact_id: String,
    pub sha256: String,
    pub format: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedArtifactQuery {
    pub command_id: String,
    pub operation_id: String,
    pub resource_id: String,
    pub artifact_id: String,
    pub kind: proto::ArtifactKind,
    pub sha256: String,
    pub format: String,
}

#[derive(Debug, Clone)]
struct Manifest {
    offer: proto::ArtifactOffer,
    state: i32,
    next_chunk: u32,
    bytes: u64,
}

impl ArtifactStore {
    pub(crate) fn agent_id(&self) -> &str {
        &self.agent_id
    }

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
        make_runtime_artifact_readable(&final_path)?;
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

    pub(crate) fn resolve_committed_image_query(
        &self,
        query: &CommittedImageQuery,
    ) -> Result<PathBuf, ArtifactStoreError> {
        if query.agent_id != self.agent_id
            || !valid_reference(&query.transfer_id)
            || !valid_reference(&query.command_id)
            || !valid_reference(&query.operation_id)
            || !valid_reference(&query.resource_id)
            || !valid_reference(&query.agent_id)
            || !valid_reference(&query.artifact_id)
            || !valid_sha256(&query.sha256)
            || !matches!(query.format.as_str(), "raw" | "qcow2")
            || query.size_bytes > MAX_ARTIFACT_BYTES
        {
            return Err(ArtifactStoreError::InvalidOffer);
        }

        let manifest_path = self.manifest_path(&query.transfer_id)?;
        let manifest_meta = match fs::symlink_metadata(&manifest_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ArtifactStoreError::Conflict);
            }
            Err(error) => return Err(ArtifactStoreError::Storage(error)),
        };
        if !manifest_meta.file_type().is_file() {
            return Err(ArtifactStoreError::UnownedPath);
        }
        let manifest = read_manifest(&manifest_path)?;
        validate_offer(&manifest.offer, &self.agent_id)?;
        if manifest.state != proto::ArtifactTransferState::Committed as i32
            || manifest.offer.kind != proto::ArtifactKind::ImageBase as i32
            || manifest.offer.transfer_id != query.transfer_id
            || manifest.offer.command_id != query.command_id
            || manifest.offer.operation_id != query.operation_id
            || manifest.offer.resource_id != query.resource_id
            || manifest.offer.agent_id != query.agent_id
            || manifest.offer.artifact_id != query.artifact_id
            || manifest.offer.sha256 != query.sha256
            || manifest.offer.format != query.format
            || (query.size_bytes != 0 && manifest.offer.size_bytes != query.size_bytes)
        {
            return Err(ArtifactStoreError::Conflict);
        }

        let path = self.final_path(&manifest.offer)?;
        verify_file(&path, &manifest.offer)?;
        Ok(path)
    }

    /// Reports the durable transfer manifests that can still be reconciled
    /// after a reconnect.  The offer identity is read from the authenticated
    /// manifest rather than reconstructed by the caller; only the current
    /// stream epoch is supplied by the registration handshake.
    pub fn artifact_statuses(
        &self,
        agent_epoch: &str,
    ) -> Result<Vec<proto::ArtifactStatus>, ArtifactStoreError> {
        if !valid_reference(agent_epoch) {
            return Err(ArtifactStoreError::InvalidOffer);
        }
        let mut statuses = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(ArtifactStoreError::Storage)? {
            let entry = entry.map_err(ArtifactStoreError::Storage)?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with('.') || !name.ends_with(".manifest") {
                continue;
            }
            if statuses.len() >= 1024 {
                return Err(ArtifactStoreError::Conflict);
            }
            let path = entry.path();
            if fs::symlink_metadata(&path)
                .map_err(ArtifactStoreError::Storage)?
                .file_type()
                .is_symlink()
            {
                return Err(ArtifactStoreError::UnownedPath);
            }
            let manifest = read_manifest(&path)?;
            validate_offer(&manifest.offer, &self.agent_id)?;
            let state = proto::ArtifactTransferState::try_from(manifest.state)
                .map_err(|_| ArtifactStoreError::CorruptManifest)?;
            if !matches!(
                state,
                proto::ArtifactTransferState::Offered
                    | proto::ArtifactTransferState::Receiving
                    | proto::ArtifactTransferState::Committed
            ) {
                return Err(ArtifactStoreError::CorruptManifest);
            }
            if state == proto::ArtifactTransferState::Committed {
                verify_file(&self.final_path(&manifest.offer)?, &manifest.offer)?;
            } else {
                let part = self.part_path(&manifest.offer.transfer_id)?;
                let metadata = fs::symlink_metadata(&part).map_err(ArtifactStoreError::Storage)?;
                if !metadata.file_type().is_file() || metadata.len() != manifest.bytes {
                    return Err(ArtifactStoreError::CorruptManifest);
                }
            }
            statuses.push(proto::ArtifactStatus {
                transfer_id: manifest.offer.transfer_id.clone(),
                command_id: manifest.offer.command_id.clone(),
                operation_id: manifest.offer.operation_id.clone(),
                resource_id: manifest.offer.resource_id.clone(),
                agent_id: self.agent_id.clone(),
                agent_epoch: agent_epoch.to_owned(),
                contiguous_bytes: manifest.bytes,
                next_chunk_index: manifest.next_chunk,
                state: state as i32,
            });
        }
        statuses.sort_by(|left, right| left.transfer_id.cmp(&right.transfer_id));
        Ok(statuses)
    }

    /// Resolves a committed artifact without reconstructing a transfer offer
    /// from incomplete command data. The complete offer remains the
    /// authority; this lookup only finds a single manifest whose authenticated
    /// command/resource identity and declared artifact metadata match.
    pub fn resolve_committed_artifact(
        &self,
        query: &CommittedArtifactQuery,
    ) -> Result<PathBuf, ArtifactStoreError> {
        if !valid_reference(&query.command_id)
            || !valid_reference(&query.operation_id)
            || !valid_reference(&query.resource_id)
            || !valid_reference(&query.artifact_id)
            || !valid_sha256(&query.sha256)
            || !valid_reference(&query.format)
        {
            return Err(ArtifactStoreError::InvalidOffer);
        }
        let mut match_path = None;
        for entry in fs::read_dir(&self.root).map_err(ArtifactStoreError::Storage)? {
            let entry = entry.map_err(ArtifactStoreError::Storage)?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with('.') || !name.ends_with(".manifest") {
                continue;
            }
            let manifest_path = entry.path();
            if manifest_path.is_symlink() {
                return Err(ArtifactStoreError::UnownedPath);
            }
            let manifest = read_manifest(&manifest_path)?;
            let offer = &manifest.offer;
            if offer.command_id == query.command_id
                && offer.operation_id == query.operation_id
                && offer.resource_id == query.resource_id
                && offer.agent_id == self.agent_id
                && offer.artifact_id == query.artifact_id
                && offer.kind == query.kind as i32
                && offer.sha256 == query.sha256
                && offer.format == query.format
            {
                if match_path.is_some() {
                    return Err(ArtifactStoreError::Conflict);
                }
                match_path = Some(offer.clone());
            }
        }
        let offer = match_path.ok_or(ArtifactStoreError::Conflict)?;
        self.resolve(&offer)
    }

    /// Removes only config-drive transfer state owned by this agent and
    /// resource. Shared final content remains until every owned manifest
    /// referring to it has been removed.
    pub fn cleanup_config_drive_for_resource(
        &self,
        resource_id: &str,
    ) -> Result<ArtifactCleanup, ArtifactStoreError> {
        if !valid_reference(resource_id) {
            return Err(ArtifactStoreError::InvalidOffer);
        }
        let mut manifests = Vec::new();
        let mut final_references = std::collections::HashMap::<PathBuf, usize>::new();
        for entry in fs::read_dir(&self.root).map_err(ArtifactStoreError::Storage)? {
            let entry = entry.map_err(ArtifactStoreError::Storage)?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with('.') || !name.ends_with(".manifest") {
                continue;
            }
            let manifest_path = entry.path();
            let metadata =
                fs::symlink_metadata(&manifest_path).map_err(ArtifactStoreError::Storage)?;
            if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                return Err(ArtifactStoreError::UnownedPath);
            }
            let manifest = read_manifest(&manifest_path)?;
            validate_offer_identity(&manifest.offer, &self.agent_id)?;
            let state = proto::ArtifactTransferState::try_from(manifest.state)
                .map_err(|_| ArtifactStoreError::CorruptManifest)?;
            if !matches!(
                state,
                proto::ArtifactTransferState::Offered
                    | proto::ArtifactTransferState::Receiving
                    | proto::ArtifactTransferState::Committed
            ) {
                return Err(ArtifactStoreError::CorruptManifest);
            }
            if state == proto::ArtifactTransferState::Committed {
                *final_references
                    .entry(self.final_path(&manifest.offer)?)
                    .or_default() += 1;
            }
            if manifest.offer.resource_id == resource_id
                && manifest.offer.kind == proto::ArtifactKind::ConfigDriveIso as i32
            {
                manifests.push((manifest_path, manifest, state));
            }
        }
        if manifests.is_empty() {
            return Ok(ArtifactCleanup::AlreadyAbsent);
        }
        let mut removed = false;
        for (manifest_path, manifest, state) in manifests {
            let part = self.part_path(&manifest.offer.transfer_id)?;
            remove_owned_file(&part)?;
            if state == proto::ArtifactTransferState::Committed {
                let final_path = self.final_path(&manifest.offer)?;
                if final_references.get(&final_path) == Some(&1) {
                    remove_owned_file(&final_path)?;
                }
            }
            remove_owned_file(&manifest_path)?;
            removed = true;
        }
        Ok(if removed {
            ArtifactCleanup::Removed
        } else {
            ArtifactCleanup::AlreadyAbsent
        })
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
    validate_offer_identity(offer, agent)?;
    if offer.expires_at_unix_ms <= crate::unix_ms() {
        return Err(ArtifactStoreError::InvalidOffer);
    }
    Ok(())
}

/// Validates offer identity and shape without the admission expiry fence.
///
/// Expiry fences new transfer admission; it must not prevent cleanup of a
/// committed artifact whose offer expired after the transfer completed, or a
/// server could never be deleted once its create deadline passed.
fn validate_offer_identity(
    offer: &proto::ArtifactOffer,
    agent: &str,
) -> Result<(), ArtifactStoreError> {
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
        || offer.chunk_count > MAX_ARTIFACT_CHUNKS
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

fn remove_owned_file(path: &Path) -> Result<(), ArtifactStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                return Err(ArtifactStoreError::UnownedPath);
            }
            fs::remove_file(path).map_err(ArtifactStoreError::Storage)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ArtifactStoreError::Storage(error)),
    }
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

fn make_runtime_artifact_readable(path: &Path) -> Result<(), ArtifactStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o640))
            .map_err(ArtifactStoreError::Storage)?;
    }
    Ok(())
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
            transfer_id: Uuid::new_v5(&Uuid::NAMESPACE_URL, b"command-1:0:image-1").to_string(),
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

    fn query(offer: &proto::ArtifactOffer) -> CommittedImageQuery {
        CommittedImageQuery {
            transfer_id: offer.transfer_id.clone(),
            command_id: offer.command_id.clone(),
            operation_id: offer.operation_id.clone(),
            resource_id: offer.resource_id.clone(),
            agent_id: offer.agent_id.clone(),
            artifact_id: offer.artifact_id.clone(),
            sha256: offer.sha256.clone(),
            format: offer.format.clone(),
            size_bytes: offer.size_bytes,
        }
    }

    fn resolve(
        store: &ArtifactStore,
        query: &CommittedImageQuery,
    ) -> Result<PathBuf, ArtifactStoreError> {
        store.resolve_committed_image_query(query)
    }

    fn committed_fixture(root: &Path) -> (ArtifactStore, proto::ArtifactOffer, Vec<u8>, PathBuf) {
        let (store, offer, content) = fixture(root);
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
        let path = store
            .finish(
                &offer,
                &proto::ArtifactEnd {
                    transfer_id: offer.transfer_id.clone(),
                    sha256: offer.sha256.clone(),
                    size_bytes: offer.size_bytes,
                },
            )
            .unwrap()
            .path
            .unwrap();
        (store, offer, content, path)
    }

    #[test]
    fn expired_offer_is_rejected_at_store_boundary() {
        let root = std::env::temp_dir().join(format!("o3k-artifact-expired-{}", Uuid::now_v7()));
        let (store, mut offer, _) = fixture(&root);
        offer.expires_at_unix_ms = 1;
        assert!(matches!(
            store.begin(&offer),
            Err(ArtifactStoreError::InvalidOffer)
        ));
        std::fs::remove_dir_all(root).ok();
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
        assert_eq!(
            fs::read(
                reopened
                    .resolve_committed_artifact(&CommittedArtifactQuery {
                        command_id: offer.command_id.clone(),
                        operation_id: offer.operation_id.clone(),
                        resource_id: offer.resource_id.clone(),
                        artifact_id: offer.artifact_id.clone(),
                        kind: proto::ArtifactKind::ImageBase,
                        sha256: offer.sha256.clone(),
                        format: offer.format.clone(),
                    },)
                    .unwrap()
            )
            .unwrap(),
            content
        );
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

    #[test]
    fn resolves_committed_image_by_exact_identity() {
        let root = std::env::temp_dir().join(format!("o3k-artifact-{}", Uuid::now_v7()));
        let (store, offer, content, expected_path) = committed_fixture(&root);
        let path = resolve(&store, &query(&offer)).unwrap();
        assert_eq!(path, expected_path);
        assert_eq!(fs::read(path).unwrap(), content);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn committed_image_lookup_rejects_identity_mismatch() {
        let root = std::env::temp_dir().join(format!("o3k-artifact-{}", Uuid::now_v7()));
        let (store, offer, _content, _path) = committed_fixture(&root);
        for mutate in [
            |value: &mut CommittedImageQuery| value.transfer_id = "other-transfer".into(),
            |value: &mut CommittedImageQuery| value.command_id = "command-2".into(),
            |value: &mut CommittedImageQuery| value.operation_id = "operation-2".into(),
            |value: &mut CommittedImageQuery| value.resource_id = "resource-2".into(),
            |value: &mut CommittedImageQuery| value.artifact_id = "other-image".into(),
            |value: &mut CommittedImageQuery| value.sha256 = "0".repeat(64),
            |value: &mut CommittedImageQuery| value.format = "qcow2".into(),
            |value: &mut CommittedImageQuery| value.size_bytes += 1,
        ] {
            let mut value = query(&offer);
            mutate(&mut value);
            assert!(matches!(
                resolve(&store, &value),
                Err(ArtifactStoreError::Conflict)
            ));
        }
        let mut wrong_agent = query(&offer);
        wrong_agent.agent_id = "other-agent".into();
        assert!(matches!(
            resolve(&store, &wrong_agent),
            Err(ArtifactStoreError::InvalidOffer)
        ));
        assert!(matches!(
            resolve(
                &store,
                &CommittedImageQuery {
                    size_bytes: offer.size_bytes + 1,
                    ..query(&offer)
                }
            ),
            Err(ArtifactStoreError::Conflict)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn committed_image_lookup_rejects_tampering() {
        let root = std::env::temp_dir().join(format!("o3k-artifact-{}", Uuid::now_v7()));
        let (store, offer, _content, path) = committed_fixture(&root);
        fs::write(&path, b"tampered").unwrap();
        assert!(matches!(
            resolve(&store, &query(&offer)),
            Err(ArtifactStoreError::DigestMismatch)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn committed_image_lookup_rejects_symlinked_and_foreign_state() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("o3k-artifact-{}", Uuid::now_v7()));
        let (store, offer, content, path) = committed_fixture(&root);
        let outside = root.with_extension("outside");
        fs::write(&outside, content).unwrap();
        fs::remove_file(&path).unwrap();
        symlink(&outside, &path).unwrap();
        assert!(matches!(
            resolve(&store, &query(&offer)),
            Err(ArtifactStoreError::DigestMismatch)
        ));
        fs::remove_file(&path).unwrap();

        let manifest = store.manifest_path(&offer.transfer_id).unwrap();
        let foreign = root.join("foreign.manifest");
        fs::copy(&manifest, &foreign).unwrap();
        fs::remove_file(&manifest).unwrap();
        symlink(&foreign, &manifest).unwrap();
        assert!(matches!(
            resolve(&store, &query(&offer)),
            Err(ArtifactStoreError::UnownedPath)
        ));
        fs::remove_file(&manifest).unwrap();
        fs::remove_file(&foreign).unwrap();
        fs::remove_file(&outside).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn committed_image_lookup_survives_store_restart() {
        let root = std::env::temp_dir().join(format!("o3k-artifact-{}", Uuid::now_v7()));
        let (_store, offer, content, expected_path) = committed_fixture(&root);
        let reopened = ArtifactStore::open(&root, "agent-1").unwrap();
        let path = resolve(&reopened, &query(&offer)).unwrap();
        assert_eq!(path, expected_path);
        assert_eq!(fs::read(path).unwrap(), content);
    }

    #[test]
    fn reconnect_statuses_are_manifest_bound_and_deterministically_ordered() {
        let root = std::env::temp_dir().join(format!("o3k-artifact-{}", Uuid::now_v7()));
        let (store, offer, content) = fixture(&root);
        store.begin(&offer).unwrap();
        store
            .accept_chunk(
                &offer,
                &proto::ArtifactChunk {
                    transfer_id: offer.transfer_id.clone(),
                    chunk_index: 0,
                    offset_bytes: 0,
                    data: content[..4].to_vec(),
                    chunk_sha256: digest(&content[..4]),
                },
            )
            .unwrap();

        let statuses = store.artifact_statuses("epoch-2").unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].transfer_id, offer.transfer_id);
        assert_eq!(statuses[0].command_id, offer.command_id);
        assert_eq!(statuses[0].operation_id, offer.operation_id);
        assert_eq!(statuses[0].resource_id, offer.resource_id);
        assert_eq!(statuses[0].agent_id, offer.agent_id);
        assert_eq!(statuses[0].agent_epoch, "epoch-2");
        assert_eq!(
            statuses[0].state,
            proto::ArtifactTransferState::Receiving as i32
        );
        assert_eq!(statuses[0].contiguous_bytes, 4);
        assert_eq!(statuses[0].next_chunk_index, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_drive_cleanup_is_owned_idempotent_and_preserves_foreign_resources() {
        let root = std::env::temp_dir().join(format!("o3k-artifact-cleanup-{}", Uuid::now_v7()));
        let (store, mut offer, content) = fixture(&root);
        offer.kind = proto::ArtifactKind::ConfigDriveIso as i32;
        offer.format = "iso".to_owned();
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
        store
            .finish(
                &offer,
                &proto::ArtifactEnd {
                    transfer_id: offer.transfer_id.clone(),
                    sha256: offer.sha256.clone(),
                    size_bytes: offer.size_bytes,
                },
            )
            .unwrap();
        assert_eq!(
            store
                .cleanup_config_drive_for_resource("resource-1")
                .unwrap(),
            ArtifactCleanup::Removed
        );
        assert_eq!(
            store
                .cleanup_config_drive_for_resource("resource-1")
                .unwrap(),
            ArtifactCleanup::AlreadyAbsent
        );
        assert_eq!(
            store
                .cleanup_config_drive_for_resource("resource-2")
                .unwrap(),
            ArtifactCleanup::AlreadyAbsent
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_drive_cleanup_succeeds_after_the_offer_expiry() {
        // Artifact offers expire at the create command deadline. Deleting a
        // server later must still clean up its committed config-drive:
        // expiry fences transfer admission, not owned-artifact deletion.
        let root = std::env::temp_dir().join(format!("o3k-artifact-expiry-{}", Uuid::now_v7()));
        let (store, mut offer, content) = fixture(&root);
        offer.kind = proto::ArtifactKind::ConfigDriveIso as i32;
        offer.format = "iso".to_owned();
        offer.expires_at_unix_ms = crate::unix_ms() + 5_000;
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
        store
            .finish(
                &offer,
                &proto::ArtifactEnd {
                    transfer_id: offer.transfer_id.clone(),
                    sha256: offer.sha256.clone(),
                    size_bytes: offer.size_bytes,
                },
            )
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(
            store
                .cleanup_config_drive_for_resource("resource-1")
                .unwrap(),
            ArtifactCleanup::Removed
        );
        fs::remove_dir_all(root).unwrap();
    }
}
