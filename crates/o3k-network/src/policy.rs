//! Provider-facing policy contracts retained at the stable module path.
//!
//! Linux nftables realization lives under [`crate::linux_fabric`].

pub use crate::linux_fabric::policy_realization::{
    PolicyEndpoint, PolicyNetworkError, StatefulPolicyProvider,
};
