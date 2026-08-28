#!/usr/bin/env python3
"""Extract o3k-image/src/lib.rs into modules. Exact line-based extraction."""

import re
from pathlib import Path

SRC = Path("crates/o3k-image/src/lib.rs")
DST = Path("crates/o3k-image/src")
lines = SRC.read_text().splitlines(keepends=True)

# ─── Module definitions: (name, start_line_1based, end_line_1based_exclusive) ───
# These boundaries are 1-based, inclusive start, exclusive end
modules = {
    # Types: ImageStatus, ImageRecord, ImageArtifact, CachedImageArtifact, ImageError
    "record": (62, 141), 
    # ImageCache struct + impl (ends at blank line before validate_verified_base)
    "cache": (141, 554),
    # qemu_img adapter (run_qemu_img + read_bounded_output + is_checksum)
    "qemu_img": (1035, 1142),
    # ImageService struct + impl + content_path + image_from_store
    "service": (1142, 1753),
    # Internal helpers: everything between cache and qemu_img
    "internal": (554, 1035),
}

# Targets that need transforms: function names to make pub(crate)
# These are private functions referenced by tests or cross-module
PUB_CRATE_FNS_IN_INTERNAL = [
    "validate_verified_base",
    "reject_qcow2_dependencies",
    "validate_qcow2_structure",
    "read_exact_at",
    "be_u32",
    "be_u64",
    "ensure_managed_directory",
    "remove_temporary_files",
    "is_base_temporary",
    "is_overlay_temporary",
    "is_upload_temporary",
    "verify_overlay",
    "overlay_virtual_size",
    "verify_image_format",
    "verify_qcow2_consistency",
]

PUB_CRATE_FNS_IN_CACHE = [
    # All ImageCache methods are already pub
]

PUB_CRATE_FNS_IN_QEMU_IMG = [
    "run_qemu_img",
    "read_bounded_output",
    "is_checksum",
]

def make_pub_crate(text, fn_name):
    """Change `fn fn_name(` to `pub(crate) fn fn_name(`."""
    return re.sub(
        rf'^(?P<indent>[ \t]*)fn {re.escape(fn_name)}\(',
        lambda m: m.group(1) + 'pub(crate) fn ' + fn_name + '(',
        text,
        flags=re.MULTILINE
    )

def extract(name, start, end):
    """Extract lines and apply transforms."""
    raw = "".join(lines[start-1:end-1])
    
    # Add doc comment
    if name == "record":
        raw = "//! Image domain types: status, record, artifact, error.\n\n" + raw
    elif name == "cache":
        raw = "//! Content-addressed image cache for verified base images and overlays.\n\n" + raw
    elif name == "qemu_img":
        raw = """\
//! HOST EXECUTION ADAPTER — not part of application domain.
//!
//! This module wraps `qemu-img` subprocess invocations with resource limits
//! (timeout, output size, address space, open files). It is a bounded host/OS
//! execution adapter that belongs conceptually in the infrastructure provider
//! layer. The long-term target is extraction behind a provider port.

""" + raw
        # Remove the original imports and O3K_TEST_QEMU_IMG_FAIL const from the extracted text
        # since this module needs its own imports
    elif name == "service":
        raw = "//! Image service: CRUD, upload, download, authorization, audit.\n\n" + raw
    elif name == "internal":
        raw = "//! Internal helper functions shared between ImageCache and ImageService.\n\n" + raw
    
    # Apply pub(crate) transforms
    if name == "internal":
        for fn in PUB_CRATE_FNS_IN_INTERNAL:
            raw = make_pub_crate(raw, fn)
        # Make TemporaryKind pub(crate)
        raw = re.sub(
            r'^enum TemporaryKind',
            'pub(crate) enum TemporaryKind',
            raw,
            flags=re.MULTILINE
        )
    elif name == "qemu_img":
        for fn in PUB_CRATE_FNS_IN_QEMU_IMG:
            raw = make_pub_crate(raw, fn)
    
    # For cache: make the QEMU_IMG constants pub(crate) since they're used
    if name == "cache":
        raw = re.sub(
            r'^(const QEMU_IMG_)',
            r'pub(crate) const \1',
            raw,
            flags=re.MULTILINE
        )
    
    (DST / f"{name}.rs").write_text(raw)
    print(f"  {name}.rs: {end-start} lines")

# Extract all modules
print("Extracting modules...")
for name, (start, end) in modules.items():
    extract(name, start, end)

# Now write the new lib.rs as a re-export hub
lib_content = """\
//! Image service: domain types, content-addressed cache, service orchestration.
//!
//! Architecture:
//! - `record` — domain types (ImageStatus, ImageRecord, ImageArtifact, ImageError)
//! - `cache` — content-addressed image cache (ImageCache)
//! - `service` — ImageService (CRUD, upload, download, authorization, audit)
//! - `internal` — shared helper functions
//! - `qemu_img` — HOST EXECUTION ADAPTER for qemu-img subprocess

pub mod record;
pub mod cache;
pub mod service;
pub(crate) mod internal;
pub(crate) mod qemu_img;

// Re-exports: preserve all existing public API paths.
// (These types were originally defined at the crate root.)
pub use record::{
    CachedImageArtifact, ImageArtifact, ImageError, ImageRecord, ImageStatus,
};
pub use cache::ImageCache;
pub use service::ImageService;

// Public constants
pub use self::{
    DEFAULT_MAX_CACHE_BYTES, DEFAULT_MAX_UPLOAD_BYTES,
};

const DEFAULT_MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_CACHE_BYTES: u64 = 10 * 1024 * 1024 * 1024;

"""

# Extract the test module (lines 1753-3362)
# The test module starts with `use super::*;` which still works since
# lib.rs re-exports all public types
test_raw = "".join(lines[1752:])  # line 1753 is 0-based index 1752

# Fix references: replace `super::` with `crate::` for internal function calls
# This is needed because the test functions reference private functions directly
# The test module had `use super::*;` which imported everything from the parent module
# Now the parent module is lib.rs which re-exports public types
# Internal functions need `crate::internal::` or `crate::qemu_img::` prefix

# The test uses `use super::*;` which imports re-exports from lib.rs
# But it also calls private functions like `run_qemu_img(...)` directly
# These need `crate::qemu_img::run_qemu_img(...)` etc.

# Replace direct function calls with their module-qualified versions
fns_in_tests = {
    # line numbers of test locations that reference these functions
    "run_qemu_img": "crate::qemu_img::run_qemu_img",
    "reject_qcow2_dependencies": "crate::internal::reject_qcow2_dependencies",
    "validate_qcow2_structure": "crate::internal::validate_qcow2_structure",
    "ensure_managed_directory": "crate::internal::ensure_managed_directory",
    "verify_image_format": "crate::internal::verify_image_format",
    "verify_qcow2_consistency": "crate::internal::verify_qcow2_consistency",
    "read_exact_at": "crate::internal::read_exact_at",
    "be_u64": "crate::internal::be_u64",
    "be_u32": "crate::internal::be_u32",
    "read_bounded_output": "crate::qemu_img::read_bounded_output",
    "is_checksum": "crate::qemu_img::is_checksum",
    "validate_verified_base": "crate::internal::validate_verified_base",
    "verify_overlay": "crate::internal::verify_overlay",
    "overlay_virtual_size": "crate::internal::overlay_virtual_size",
    "remove_temporary_files": "crate::internal::remove_temporary_files",
    "QCOW2_CLUSTER_OFFSET_MASK": "crate::record::QCOW2_CLUSTER_OFFSET_MASK",
    "QCOW2_REFCOUNT_BLOCK_OFFSET_MASK": "crate::record::QCOW2_REFCOUNT_BLOCK_OFFSET_MASK",
    "QCOW2_INCOMPATIBLE_ALLOWED": "crate::record::QCOW2_INCOMPATIBLE_ALLOWED",
    "QCOW2_VERSION_2_HEADER": "crate::record::QCOW2_VERSION_2_HEADER",
    "QCOW2_VERSION_3_HEADER": "crate::record::QCOW2_VERSION_3_HEADER",
    "QCOW2_MAX_DISK_SIZE": "crate::record::QCOW2_MAX_DISK_SIZE",
    "TemporaryKind": "crate::internal::TemporaryKind",
}

# Actually, the test code uses these functions as bare names (e.g. `run_qemu_img(...)`)
# NOT as `super::run_qemu_img(...)`. Since `use super::*` brought everything into scope,
# and the private functions are now in sub-modules, I need to add use statements.

# Simple approach: add use statements for internal and qemu_img modules
test_raw = test_raw.replace(
    "use super::*;",
    "use super::*;\n    use crate::internal::*;\n    use crate::qemu_img::*;"
)

# Can't use .format() because test_raw contains { and } characters
# Use concatenation instead
full_lib = lib_content + "\n" + test_raw

(DST / "lib.rs").write_text(full_lib)
print(f"  lib.rs: rewritten as re-export hub (+ {len(test_raw.splitlines())} test lines)")
print("\nDone. Files created:")
for p in sorted(DST.glob("*.rs")):
    print(f"  {p.name}: {len(p.read_text().splitlines())} lines")
