use std::{env, fs, path::PathBuf};

use o3k_network::{NodeNetworkPlan, canonical_plan_fingerprint};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::args()
        .nth(1)
        .ok_or("usage: network-plan-fingerprint <plan.json>")?;
    let path = PathBuf::from(path);
    let mut plan: NodeNetworkPlan = serde_json::from_slice(&fs::read(&path)?)?;
    plan.fingerprint_sha256 = canonical_plan_fingerprint(&plan)?;
    println!("{}", serde_json::to_string(&plan)?);
    Ok(())
}
