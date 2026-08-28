use std::{
    collections::HashSet,
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use sha2::{Digest, Sha256};
use uuid::Uuid;

pub(crate) use crate::qemu_img::{is_checksum, run_qemu_img};
use crate::record::{
    ImageError, QCOW2_CLUSTER_OFFSET_MASK, QCOW2_INCOMPATIBLE_ALLOWED, QCOW2_MAX_CLUSTER_BITS,
    QCOW2_MAX_DISK_SIZE, QCOW2_MAX_HEADER_LENGTH, QCOW2_MAX_REFCOUNT_ORDER, QCOW2_MIN_CLUSTER_BITS,
    QCOW2_REFCOUNT_BLOCK_OFFSET_MASK, QCOW2_VERSION_2_HEADER, QCOW2_VERSION_3_HEADER,
};

/// Internal helper functions shared between ImageCache and ImageService.
pub(crate) fn validate_verified_base(
    qemu_img: &Path,
    base_dir: &Path,
    base: &Path,
    max_bytes: u64,
) -> Result<(), ImageError> {
    if base.parent() != Some(base_dir) {
        return Err(ImageError::InvalidPath);
    }
    let name = base
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(ImageError::InvalidPath)?;
    let (checksum, format) = name.rsplit_once('.').ok_or(ImageError::InvalidPath)?;
    if !is_checksum(checksum) || !matches!(format, "raw" | "qcow2") {
        return Err(ImageError::InvalidPath);
    }
    let metadata = fs::symlink_metadata(base).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ImageError::NotFound
        } else {
            ImageError::Storage(error)
        }
    })?;
    if !metadata.file_type().is_file() || metadata.len() > max_bytes {
        return Err(ImageError::InvalidPath);
    }
    let content = fs::read(base).map_err(ImageError::Storage)?;
    if content.len() as u64 != metadata.len()
        || format!("{:x}", Sha256::digest(&content)) != checksum
    {
        return Err(ImageError::ChecksumMismatch);
    }
    if format == "qcow2" {
        reject_qcow2_dependencies(base)?;
        verify_image_format(qemu_img, base, format)?;
        // Full metadata self-consistency check (refcounts, overlaps) before
        // an overlay is derived from the base and handed to libvirt.
        verify_qcow2_consistency(qemu_img, base)?;
    }
    Ok(())
}

/// Rejects qcow2 backing and external-data references by inspecting only the
/// fixed-size qcow2 header. This runs before qemu-img so an uploaded image
/// cannot make the helper open a tenant-controlled host path while discovering
/// that the image is unsafe.
pub(crate) fn reject_qcow2_dependencies(path: &Path) -> Result<(), ImageError> {
    let mut file = fs::File::open(path).map_err(ImageError::Storage)?;
    let mut header = [0_u8; 104];
    let count = file.read(&mut header).map_err(ImageError::Storage)?;
    if count < 32 || &header[..4] != b"QFI\xfb" {
        // qemu-img remains the format authority for malformed/non-qcow2
        // bytes; this branch keeps injectable test helpers deterministic.
        return Ok(());
    }
    let version = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    if !matches!(version, 2 | 3) {
        return Err(ImageError::FormatVerificationFailed);
    }
    let backing_offset = u64::from_be_bytes([
        header[8], header[9], header[10], header[11], header[12], header[13], header[14],
        header[15],
    ]);
    let backing_size = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
    if backing_offset != 0 || backing_size != 0 {
        return Err(ImageError::FormatVerificationFailed);
    }
    // QCOW2 v3 incompatible feature bit 2 denotes an external data file.
    if version == 3 && count >= 80 {
        let incompatible = u64::from_be_bytes([
            header[72], header[73], header[74], header[75], header[76], header[77], header[78],
            header[79],
        ]);
        if incompatible & (1 << 2) != 0 {
            return Err(ImageError::FormatVerificationFailed);
        }
    }
    Ok(())
}

/// Structurally validates a qcow2 payload so a truncated or corrupt image is
/// rejected before its record can be activated. This is the import-time
/// gate: every on-disk structure reachable from the header -- the L1 table,
/// L2 tables, data clusters, the refcount table, and refcount blocks -- must
/// lie completely inside the `len` payload bytes, or the image could never
/// be materialized and booted. Unallocated entries (offset zero) reference
/// nothing and are allowed.
///
/// Field layout, table entry formats, and compressed cluster sizing follow
/// the QEMU qcow2 format documentation (docs/interop/qcow2.rst). The walk is
/// bounded by the payload size: tables must be inside the payload, and each
/// distinct L2 table is visited at most once.
pub(crate) fn validate_qcow2_structure(
    reader: &mut (impl Read + Seek),
    len: u64,
) -> Result<(), ImageError> {
    if len < QCOW2_VERSION_2_HEADER {
        return Err(ImageError::FormatVerificationFailed);
    }
    let mut header = [0_u8; QCOW2_VERSION_3_HEADER as usize];
    read_exact_at(reader, 0, &mut header[..QCOW2_VERSION_2_HEADER as usize])?;
    if &header[0..4] != b"QFI\xfb" {
        return Err(ImageError::FormatVerificationFailed);
    }
    let version = be_u32(&header[4..8]);
    if !matches!(version, 2 | 3) {
        return Err(ImageError::FormatVerificationFailed);
    }
    let cluster_bits = be_u32(&header[20..24]);
    if !(QCOW2_MIN_CLUSTER_BITS..=QCOW2_MAX_CLUSTER_BITS).contains(&cluster_bits) {
        return Err(ImageError::FormatVerificationFailed);
    }
    let cluster_size = 1_u64 << cluster_bits;
    // Uploaded images must be self-contained: a backing file reference would
    // make later booting depend on a host path controlled by the uploader.
    // This mirrors `reject_qcow2_dependencies` at the import boundary.
    if be_u64(&header[8..16]) != 0 || be_u32(&header[16..20]) != 0 {
        return Err(ImageError::FormatVerificationFailed);
    }
    // No key material exists to open an encrypted image, so it could never
    // boot; reject it here instead of after placement.
    if be_u32(&header[32..36]) != 0 {
        return Err(ImageError::FormatVerificationFailed);
    }
    if version == 3 {
        if len < QCOW2_VERSION_3_HEADER {
            return Err(ImageError::FormatVerificationFailed);
        }
        read_exact_at(
            reader,
            QCOW2_VERSION_2_HEADER,
            &mut header[QCOW2_VERSION_2_HEADER as usize..],
        )?;
        let header_length = u64::from(be_u32(&header[100..104]));
        if header_length < QCOW2_VERSION_3_HEADER
            || header_length % 8 != 0
            || header_length > QCOW2_MAX_HEADER_LENGTH
            || header_length > len
        {
            return Err(ImageError::FormatVerificationFailed);
        }
        if be_u32(&header[96..100]) > QCOW2_MAX_REFCOUNT_ORDER {
            return Err(ImageError::FormatVerificationFailed);
        }
        if be_u64(&header[72..80]) & !QCOW2_INCOMPATIBLE_ALLOWED != 0 {
            return Err(ImageError::FormatVerificationFailed);
        }
    }
    let disk_size = be_u64(&header[24..32]);
    if disk_size == 0 || disk_size > QCOW2_MAX_DISK_SIZE {
        return Err(ImageError::FormatVerificationFailed);
    }
    let l1_size = u64::from(be_u32(&header[36..40]));
    if l1_size == 0 {
        return Err(ImageError::FormatVerificationFailed);
    }
    let l1_table_offset = be_u64(&header[40..48]);
    // Checked arithmetic: a hostile header must never be able to wrap an
    // extent sum around u64::MAX and slip past the payload bound.
    if l1_table_offset == 0
        || !l1_table_offset.is_multiple_of(cluster_size)
        || l1_table_offset
            .checked_add(l1_size * 8)
            .is_none_or(|end| end > len)
    {
        return Err(ImageError::FormatVerificationFailed);
    }
    // The active L1 table must be able to address the entire virtual disk; a
    // smaller table would expose a truncated virtual disk. Each L1 entry
    // covers cluster_size/8 L2 entries of cluster_size bytes each.
    let covered_per_l1_entry = (cluster_size / 8) * cluster_size;
    if disk_size.div_ceil(covered_per_l1_entry) > l1_size {
        return Err(ImageError::FormatVerificationFailed);
    }
    let refcount_table_clusters = u64::from(be_u32(&header[56..60]));
    let refcount_table_offset = be_u64(&header[48..56]);
    if refcount_table_clusters == 0
        || refcount_table_offset == 0
        || !refcount_table_offset.is_multiple_of(cluster_size)
        || refcount_table_offset
            .checked_add(refcount_table_clusters * cluster_size)
            .is_none_or(|end| end > len)
    {
        return Err(ImageError::FormatVerificationFailed);
    }
    if be_u32(&header[60..64]) > 0 && be_u64(&header[64..72]) == 0 {
        return Err(ImageError::FormatVerificationFailed);
    }

    // Active L1 table: every used entry names an L2 table that must be fully
    // inside the payload.
    let mut l1 = vec![0_u8; (l1_size * 8) as usize];
    read_exact_at(reader, l1_table_offset, &mut l1)?;
    let mut visited_l2 = HashSet::new();
    for entry in l1.chunks_exact(8) {
        let l2_offset = be_u64(entry) & QCOW2_CLUSTER_OFFSET_MASK;
        if l2_offset == 0 || !visited_l2.insert(l2_offset) {
            continue;
        }
        if !l2_offset.is_multiple_of(cluster_size)
            || l2_offset
                .checked_add(cluster_size)
                .is_none_or(|end| end > len)
        {
            return Err(ImageError::FormatVerificationFailed);
        }
        // L2 table: standard entries name a whole data cluster; compressed
        // entries name an (unaligned) extent of 512-byte sectors.
        let mut l2 = vec![0_u8; cluster_size as usize];
        read_exact_at(reader, l2_offset, &mut l2)?;
        for entry in l2.chunks_exact(8) {
            let entry = be_u64(entry);
            if entry & (1_u64 << 62) != 0 {
                let offset_bits = 62 - (cluster_bits - 8);
                let offset = entry & ((1_u64 << offset_bits) - 1);
                let additional_sectors =
                    (entry >> offset_bits) & ((1_u64 << (62 - offset_bits)) - 1);
                if offset
                    .checked_add((additional_sectors + 1) * 512)
                    .is_none_or(|end| end > len)
                {
                    return Err(ImageError::FormatVerificationFailed);
                }
            } else {
                let host = entry & QCOW2_CLUSTER_OFFSET_MASK;
                if host != 0
                    && (!host.is_multiple_of(cluster_size)
                        || host.checked_add(cluster_size).is_none_or(|end| end > len))
                {
                    return Err(ImageError::FormatVerificationFailed);
                }
            }
        }
    }

    // Refcount table: every used entry names a refcount block of exactly one
    // cluster that must be fully inside the payload.
    let refcount_table_bytes = refcount_table_clusters * cluster_size;
    let mut refcount_table = vec![0_u8; refcount_table_bytes as usize];
    read_exact_at(reader, refcount_table_offset, &mut refcount_table)?;
    for entry in refcount_table.chunks_exact(8) {
        let block_offset = be_u64(entry) & QCOW2_REFCOUNT_BLOCK_OFFSET_MASK;
        if block_offset != 0
            && (!block_offset.is_multiple_of(cluster_size)
                || block_offset
                    .checked_add(cluster_size)
                    .is_none_or(|end| end > len))
        {
            return Err(ImageError::FormatVerificationFailed);
        }
    }
    Ok(())
}

pub(crate) fn read_exact_at(
    reader: &mut (impl Read + Seek),
    offset: u64,
    buffer: &mut [u8],
) -> Result<(), ImageError> {
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(ImageError::Storage)?;
    reader.read_exact(buffer).map_err(ImageError::Storage)
}

pub(crate) fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

pub(crate) fn be_u64(bytes: &[u8]) -> u64 {
    u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

pub(crate) fn ensure_managed_directory(path: &Path) -> Result<(), ImageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            #[cfg(unix)]
            // Libvirt's qemu process must traverse the managed cache to read
            // an owned overlay, but group members must not list or write it.
            // The compute service installs this subtree with the kvm group;
            // 0710 grants traversal only and leaves file read policy to the
            // individual artifact modes.
            fs::set_permissions(path, fs::Permissions::from_mode(0o2710))
                .map_err(ImageError::Storage)?;
            Ok(())
        }
        Ok(_) => Err(ImageError::InvalidPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(ImageError::Storage)?;
            #[cfg(unix)]
            fs::set_permissions(path, fs::Permissions::from_mode(0o2710))
                .map_err(ImageError::Storage)?;
            Ok(())
        }
        Err(error) => Err(ImageError::Storage(error)),
    }
}

#[derive(Clone, Copy)]
pub(crate) enum TemporaryKind {
    Base,
    Overlay,
    Upload,
}

pub(crate) fn remove_temporary_files(
    directory: &Path,
    kind: TemporaryKind,
) -> Result<(), ImageError> {
    for entry in fs::read_dir(directory).map_err(ImageError::Storage)? {
        let entry = entry.map_err(ImageError::Storage)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let matches = match kind {
            TemporaryKind::Base => is_base_temporary(&name),
            TemporaryKind::Overlay => is_overlay_temporary(&name),
            TemporaryKind::Upload => is_upload_temporary(&name),
        };
        if !matches {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(ImageError::Storage)?;
        if metadata.file_type().is_file() {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

pub(crate) fn is_base_temporary(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("base-") else {
        return false;
    };
    let Some((checksum, suffix)) = rest.split_once(".tmp-") else {
        return false;
    };
    is_checksum(checksum) && Uuid::parse_str(suffix).is_ok()
}

pub(crate) fn is_overlay_temporary(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    let Some((instance, suffix)) = rest.split_once(".tmp-") else {
        return false;
    };
    !instance.is_empty() && Uuid::parse_str(suffix).is_ok()
}

pub(crate) fn is_upload_temporary(name: &str) -> bool {
    let Some((image_id, suffix)) = name.split_once(".upload-") else {
        return false;
    };
    Uuid::parse_str(image_id).is_ok() && Uuid::parse_str(suffix).is_ok()
}

pub(crate) fn verify_overlay(
    qemu_img: &Path,
    overlay: &Path,
    base: &Path,
) -> Result<(), ImageError> {
    let expected_base = fs::canonicalize(base).map_err(|_| ImageError::OverlayFailed)?;
    let output = run_qemu_img(
        qemu_img,
        [
            "info",
            "--output=json",
            overlay.to_str().ok_or(ImageError::OverlayFailed)?,
        ],
    )
    .map_err(|_| ImageError::OverlayFailed)?;
    if !output.status.success() {
        return Err(ImageError::OverlayFailed);
    }
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| ImageError::OverlayFailed)?;
    if info.get("format").and_then(serde_json::Value::as_str) != Some("qcow2") {
        return Err(ImageError::OverlayFailed);
    }

    let mut backing_paths = Vec::new();
    for field in ["backing-filename", "full-backing-filename"] {
        if let Some(backing) = info.get(field).and_then(serde_json::Value::as_str) {
            backing_paths.push(backing);
        }
    }
    if backing_paths.is_empty() {
        return Err(ImageError::OverlayFailed);
    }
    let overlay_parent = overlay.parent().ok_or(ImageError::OverlayFailed)?;
    for backing in backing_paths {
        let reported = Path::new(backing);
        let resolved = if reported.is_absolute() {
            reported.to_path_buf()
        } else {
            overlay_parent.join(reported)
        };
        if fs::canonicalize(resolved).map_err(|_| ImageError::OverlayFailed)? != expected_base {
            return Err(ImageError::OverlayFailed);
        }
    }
    Ok(())
}

pub(crate) fn overlay_virtual_size(qemu_img: &Path, overlay: &Path) -> Result<u64, ImageError> {
    let output = run_qemu_img(
        qemu_img,
        [
            "info",
            "--output=json",
            overlay.to_str().ok_or(ImageError::OverlayFailed)?,
        ],
    )
    .map_err(|_| ImageError::OverlayFailed)?;
    if !output.status.success() {
        return Err(ImageError::OverlayFailed);
    }
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| ImageError::OverlayFailed)?;
    if info.get("format").and_then(serde_json::Value::as_str) != Some("qcow2") {
        return Err(ImageError::OverlayFailed);
    }
    info.get("virtual-size")
        .and_then(serde_json::Value::as_u64)
        .ok_or(ImageError::OverlayFailed)
}

pub(crate) fn verify_image_format(
    qemu_img: &Path,
    image: &Path,
    expected: &str,
) -> Result<(), ImageError> {
    let output = run_qemu_img(
        qemu_img,
        [
            "info",
            "--output=json",
            image.to_str().ok_or(ImageError::FormatVerificationFailed)?,
        ],
    )
    .map_err(|_| ImageError::FormatVerificationFailed)?;
    if !output.status.success() {
        return Err(ImageError::FormatVerificationFailed);
    }
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| ImageError::FormatVerificationFailed)?;
    if info.get("format").and_then(serde_json::Value::as_str) != Some(expected) {
        return Err(ImageError::FormatVerificationFailed);
    }
    // Uploaded qcow2 bytes must be self-contained.  A backing reference (or
    // an external data file) would make later libvirt/qemu access depend on a
    // host path or protocol controlled by the uploader, and a nested chain
    // would evade the managed cache's ownership and digest checks.
    if expected == "qcow2"
        && [
            "backing-filename",
            "full-backing-filename",
            "backing-filename-format",
            "data-file",
            "data-file-raw",
        ]
        .iter()
        .any(|field| info.get(field).is_some_and(|value| !value.is_null()))
    {
        return Err(ImageError::FormatVerificationFailed);
    }
    Ok(())
}

/// Runs a read-only `qemu-img check` over a verified base so a truncated or
/// metadata-inconsistent qcow2 (extents beyond the end of the file, wrong
/// refcounts, overlapping structures) is rejected before an overlay is
/// derived from it and handed to libvirt.
///
/// `qemu-img check` without `-r` never repairs or writes the image. Its exit
/// code is 0 when the image is clean, 1 when only leaked clusters were found
/// (wasted space, no data corruption), and 2 when errors were found; any
/// other outcome (signal, helper failure) also fails closed.
pub(crate) fn verify_qcow2_consistency(qemu_img: &Path, image: &Path) -> Result<(), ImageError> {
    let output = run_qemu_img(
        qemu_img,
        [
            "check",
            image.to_str().ok_or(ImageError::FormatVerificationFailed)?,
        ],
    )
    .map_err(|_| ImageError::FormatVerificationFailed)?;
    if !matches!(output.status.code(), Some(0) | Some(1)) {
        return Err(ImageError::FormatVerificationFailed);
    }
    Ok(())
}
