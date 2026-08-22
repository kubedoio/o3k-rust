//! Native API CLI commands.
//!
//! These commands talk to a running O3K native API endpoint (typically
//! served by `o3kd` at the configured endpoint).
//!
//! Architecture:
//! - `o3k service list` / `o3k service show` — discover registered services
//! - `o3k resource-type list` — discover registered resource types
//! - `o3k resource list <ns:type>` — list resources of a given type
//! - `o3k resource show <ns:type> <id>` — show a specific resource
//!
//! The underlying protocol is O3K native HTTP API (ADR-0173/SPEC-0030).

use serde_json::Value;
use std::path::Path;

use crate::HttpClient;
use crate::context::HttpResponse;
use crate::sys::SystemHttpClient;

/// Default native API base URL.
const DEFAULT_API_BASE: &str = "http://127.0.0.1:18080/o3k/v1";

/// Returns the effective API base URL from environment or default.
fn api_base() -> String {
    std::env::var("O3K_API_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_API_BASE.to_owned())
}

/// Small runtime reused for each API call (cheap: current-thread, no spawn).
fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to build tokio runtime: {e}"))
}

/// Performs a GET request against the native API and returns parsed JSON.
fn api_get(path: &str) -> Result<Value, String> {
    let base = api_base();
    let url = format!("{base}{path}");
    let client = SystemHttpClient;
    let rt = runtime()?;

    let HttpResponse { status, body, .. } = rt.block_on(client.get(&url))?;

    if status != 200 {
        return Err(format!("API returned status {status}: {body}"));
    }
    serde_json::from_str(&body).map_err(|e| format!("API response parse error: {e}"))
}

/// Lists all registered services.
pub fn list_services() -> Result<(), String> {
    let json = api_get("/services")?;
    let services = json["services"]
        .as_array()
        .ok_or_else(|| "unexpected response format: missing services array".to_owned())?;

    println!("{:<20} {:<16} {:<12}", "ID", "NAMESPACE", "OWNERSHIP");
    println!("{}", "-".repeat(50));
    for svc in services {
        let id = svc["id"].as_str().unwrap_or("?");
        let ns = svc["namespace"].as_str().unwrap_or("?");
        let ownership = svc["ownership"].as_str().unwrap_or("?");
        println!("{id:<20} {ns:<16} {ownership:<12}");
    }
    println!("\nTotal: {} service(s)", services.len());
    Ok(())
}

/// Shows details for a specific service.
pub fn show_service(name: &str) -> Result<(), String> {
    let json = api_get("/services")?;
    let services = json["services"]
        .as_array()
        .ok_or_else(|| "unexpected response format: missing services array".to_owned())?;

    let svc = services
        .iter()
        .find(|s| s["id"].as_str() == Some(name) || s["namespace"].as_str() == Some(name))
        .ok_or_else(|| format!("service '{name}' not found"))?;

    println!("Service:      {}", svc["id"].as_str().unwrap_or("?"));
    println!("Namespace:    {}", svc["namespace"].as_str().unwrap_or("?"));
    println!("Ownership:    {}", svc["ownership"].as_str().unwrap_or("?"));
    println!(
        "Version:      {}",
        svc["service_version"].as_str().unwrap_or("?")
    );
    Ok(())
}

/// Lists all registered resource types.
pub fn list_resource_types() -> Result<(), String> {
    let json = api_get("/resource-types")?;
    let rts = json["resource_types"]
        .as_array()
        .ok_or_else(|| "unexpected response format: missing resource_types array".to_owned())?;

    println!("{:<24} {:<16}", "RESOURCE TYPE", "SERVICE");
    println!("{}", "-".repeat(42));
    for rt in rts {
        let ns = rt["namespace"].as_str().unwrap_or("?");
        let name = rt["name"].as_str().unwrap_or("?");
        let svc = rt["service"].as_str().unwrap_or("?");
        println!("{ns:<8}:{name:<14} {svc:<16}");
    }
    println!("\nTotal: {} resource type(s)", rts.len());
    Ok(())
}

/// Lists resources of a given namespace:type.
pub fn list_resources(ns_type: &str) -> Result<(), String> {
    let Some((ns, type_name)) = ns_type.split_once(':') else {
        return Err("resource type must be namespace:type (e.g. compute:server)".to_owned());
    };
    let path = format!("/{ns}/{type_name}");
    let json = api_get(&path)?;
    let items = json["items"]
        .as_array()
        .map_or(&[] as &[_], |a| a.as_slice());

    println!("{:<36} {:<20} {:<12}", "ID", "OWNER", "GENERATION");
    println!("{}", "-".repeat(70));
    for item in items {
        let id = item["metadata"]["id"].as_str().unwrap_or("?");
        let owner = item["metadata"]["owner_scope"]["id"]
            .as_str()
            .unwrap_or("?");
        let generation = item["metadata"]["generation"].as_i64().unwrap_or(0);
        println!("{id:<36} {owner:<20} {generation:<12}");
    }
    println!("\nTotal: {} resource(s)", items.len());
    Ok(())
}

/// Shows a specific resource by namespace:type and id.
pub fn show_resource(ns_type: &str, id: &str) -> Result<(), String> {
    let Some((ns, type_name)) = ns_type.split_once(':') else {
        return Err("resource type must be namespace:type (e.g. compute:server)".to_owned());
    };
    let path = format!("/{ns}/{type_name}/{id}");
    let json = api_get(&path)?;

    let pretty =
        serde_json::to_string_pretty(&json).map_err(|e| format!("serialization error: {e}"))?;
    println!("{pretty}");
    Ok(())
}

pub fn create_resource(ns_type: &str, file: &Path, key: Option<&str>) -> Result<(), String> {
    let (ns, name) = ns_type
        .split_once(':')
        .ok_or("resource type must be namespace:type")?;
    let body =
        std::fs::read_to_string(file).map_err(|e| format!("cannot read create file: {e}"))?;
    if body.len() > 1024 * 1024 {
        return Err("create file exceeds 1 MiB limit".to_owned());
    }
    let _: Value = serde_json::from_str(&body).map_err(|e| format!("invalid JSON: {e}"))?;
    let client = SystemHttpClient;
    let rt = runtime()?;
    let response = rt.block_on(client.post_json_with_idempotency(
        &format!("{}/{ns}/{name}", api_base()),
        &body,
        key,
    ))?;
    if response.status != 201 && response.status != 202 {
        return Err(format!(
            "API returned status {}: {}",
            response.status, response.body
        ));
    }
    println!("{}", response.body);
    Ok(())
}

pub fn delete_resource(ns_type: &str, id: &str, key: Option<&str>) -> Result<(), String> {
    let (ns, name) = ns_type
        .split_once(':')
        .ok_or("resource type must be namespace:type")?;
    let client = SystemHttpClient;
    let rt = runtime()?;
    let response = rt.block_on(
        client.delete_with_idempotency(&format!("{}/{ns}/{name}/{id}", api_base()), key),
    )?;
    if response.status != 202 && response.status != 204 {
        return Err(format!(
            "API returned status {}: {}",
            response.status, response.body
        ));
    }
    if !response.body.is_empty() {
        println!("{}", response.body);
    }
    Ok(())
}

/// Prints the help text for native commands.
pub fn print_help() {
    println!("o3k native API commands:");
    println!("  o3k service list                         list registered services");
    println!("  o3k service show <service>                show service details");
    println!("  o3k resource-type list                    list known resource types");
    println!("  o3k resource list <ns:type>               list resources of a type");
    println!("  o3k resource show <ns:type> <id>          show a specific resource");
    println!();
    println!("Environment:");
    println!("  O3K_API_URL   native API base URL (default: {DEFAULT_API_BASE})");
}
