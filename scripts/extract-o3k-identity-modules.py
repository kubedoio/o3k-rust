#!/usr/bin/env python3
"""Extract o3k-identity/src/lib.rs into types.rs + service.rs modules."""

from pathlib import Path

SRC = Path("crates/o3k-identity/src/lib.rs")
DST = Path("crates/o3k-identity/src")
lines = SRC.read_text().splitlines(keepends=True)

# Line ranges (1-based, inclusive start, exclusive end)
# [1-23]     Imports + type alias
# [24-465]   Domain types: Secret -> ExtraProjectSeed
# [466-1432] seed_identity_defaults -> TokenService -> VerifiedToken + helpers
# [1433-1556] pub mod testkit
# [1557-2192] mod tests

ranges = {
    "types": (24, 473),
    "service": (473, 1432),
}

def extract(name, start, end):
    raw = "".join(lines[start-1:end-1])
    
    if name == "types":
        raw = """\
//! Identity domain types: auth, tokens, projects, users, services, passwords.

use std::{fmt, time::SystemTime};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use o3k_kernel::AuthContext;

""" + raw
    elif name == "service":
        raw = """\
//! Token service: issue, verify, snapshot, bootstrap, signing.

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD},
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

use o3k_kernel::{
    AuthContext, OwnershipScope, Principal, PrincipalId, ScopeId, ServicePrincipal, UserPrincipal,
};
use o3k_store::{IdentityRepository, StoreError};

use crate::types::{
    Auth, AuthError, BootstrapConfig, ExtraProjectSeed, Identity, IdentitySnapshot,
    PasswordHash, ProjectDetails, Secret, TokenDetails, TokenRequest, TokenResponse,
    VerifiedToken,
};

type HmacSha256 = Hmac<Sha256>;

""" + raw
        for fn_name in ["store_auth_error", "now_rfc3339", "sign", "format_time", "civil_date"]:
            raw = raw.replace(f"\nfn {fn_name}", f"\npub(crate) fn {fn_name}")

    (DST / f"{name}.rs").write_text(raw)
    print(f"  {name}.rs: {end-start} lines")

print("Extracting identity modules...")
for name, (start, end) in ranges.items():
    extract(name, start, end)

# Write new lib.rs
lib_content = """\
//! Identity/Keystone-compatible authentication, token, catalog, and IAM.

pub mod types;
pub mod service;

pub use service::{BootstrapConfig, ExtraProjectSeed, TokenService, VerifiedToken,
    seed_identity_defaults};
pub use types::{
    Auth, AuthError, DomainDetails, DomainReference, EndpointDetails, Identity,
    IdentitySnapshot, PasswordHash, ProjectDetails, ProjectReference, RoleDetails, Scope,
    Secret, ServiceDetails, SnapshotAssignment, SnapshotDomain, SnapshotEndpoint,
    SnapshotProject, SnapshotRegion, SnapshotRole, SnapshotService, SnapshotUser,
    TokenDetails, TokenIdentity, TokenRequest, TokenResponse, UserDetails, UserReference,
};

"""

# Add testkit module: inner content (original lines 1435-1555)
testkit_inner = "".join(lines[1434:1555])
lib_content += f"""pub mod testkit {{
    use super::*;

{testkit_inner}
}}

"""

# Add test module (original lines 1556-2192: the blank line + tests)
# Line 1556 is the closing `}` of testkit — already handled by the f-string.
# Line 1557 is blank, line 1558 is `#[cfg(test)]`
lib_content += "".join(lines[1556:])

(DST / "lib.rs").write_text(lib_content)
print(f"  lib.rs: re-export hub + testkit + tests")

print("\nDone.")
for p in sorted(DST.glob("*.rs")):
    print(f"  {p.name}: {len(p.read_text().splitlines())} lines")
