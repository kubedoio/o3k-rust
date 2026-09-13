//! Production-composition convergence tests for native location discovery
//! (issue #887). These prove `/o3k/v1/regions` and the placement metadata on
//! `/o3k/v1/resource-types` are reachable through the real production router
//! `o3k_api::router_with_state` — the same router that `o3kd` serves — and not
//! only through the standalone `o3k_native_api::router`.

use axum::body::{Body, to_bytes};
use http::{Request, StatusCode};
use o3k_kernel::resource::ResourceType;
use o3k_kernel::{
    ActionId, ManifestController, ManifestRegistry, RegisteredResourceType, ResourceScope,
    ServiceManifest, ServiceOwnership,
};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

/// Canonical location topology used across these tests. This is deployment
/// *configuration*, not a hard-coded production fixture — the production
/// default has no regions configured.
fn canonical_locations() -> Result<o3k_kernel::LocationRegistry, String> {
    use o3k_kernel::{AvailabilityDomain, RegionDeclaration};
    o3k_kernel::LocationRegistry::from_declarations(vec![
        RegionDeclaration {
            id: "region-a".to_owned(),
            availability_domains: vec![
                AvailabilityDomain {
                    id: "az-1".to_owned(),
                },
                AvailabilityDomain {
                    id: "az-2".to_owned(),
                },
            ],
        },
        RegionDeclaration {
            id: "region-b".to_owned(),
            availability_domains: vec![AvailabilityDomain {
                id: "az-3".to_owned(),
            }],
        },
    ])
    .map_err(|error| error.to_string())
}

fn regional_manifest() -> ServiceManifest {
    ServiceManifest {
        manifest_version: 1,
        service_id: "compute".to_owned(),
        namespace: "compute".to_owned(),
        service_version: "0.4.0".to_owned(),
        ownership: ServiceOwnership::O3kImplemented,
        resource_types: vec![RegisteredResourceType {
            resource_type: ResourceType::new_unchecked("compute", "server"),
            schema_version: "v1".to_owned(),
            collection: None,
            scope: ResourceScope::Tenant,
            operations: std::collections::HashMap::from([
                (
                    "list".to_owned(),
                    ActionId::new_unchecked("compute", "ListServers"),
                ),
                (
                    "create".to_owned(),
                    ActionId::new_unchecked("compute", "CreateServer"),
                ),
            ]),
        }],
        actions: vec![
            "compute:ListServers".to_owned(),
            "compute:CreateServer".to_owned(),
        ],
        capabilities: vec![],
        dependencies: vec![],
        quota_dimensions: vec![],
        // References the canonical registry only — the manifest is a placement
        // filter, never an independent location authority.
        regions: vec!["region-a".to_owned(), "region-b".to_owned()],
        availability_domains: vec!["az-1".to_owned()],
        controller: Some(ManifestController {
            mode: "in-process".to_owned(),
            protocol: "in-process".to_owned(),
            protocol_version: "1.0".to_owned(),
            service_principal: None,
        }),
        health: None,
    }
}

/// Builds the production router (`o3k_api::router_with_state`) wired with a
/// manifest registry plus canonical location topology — the same composition
/// path `o3kd` follows via `NativeApiState::with_locations`.
fn production_router() -> Result<axum::Router, String> {
    let mut registry = ManifestRegistry::new();
    registry
        .register(regional_manifest())
        .map_err(|error| error.to_string())?;
    registry
        .register_in_process_controller("compute", true, None)
        .map_err(|error| error.to_string())?;
    let native = o3k_native_api::NativeApiState::new(
        Some(registry),
        o3k_native_api::pagination::CursorConfig::default(),
        None,
        None,
        None,
        None,
    )?
    .with_locations(Arc::new(o3k_native_api::topology::TopologyGuard::new(
        canonical_locations()?,
    )))
    .with_authorizer(Arc::new(o3k_kernel::StaticAuthorizer::standard()));
    Ok(o3k_api::router_with_state(
        o3k_api::AppState::new().with_native_api(native),
    ))
}

async fn get_json(
    app: axum::Router,
    uri: &str,
) -> Result<(StatusCode, Value), Box<dyn std::error::Error>> {
    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await?)?;
    Ok((status, body))
}

fn string_list(value: &Value, key: &str) -> Result<Vec<String>, String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .or_else(|| item.get("id").and_then(Value::as_str).map(str::to_owned))
                        .unwrap_or_default()
                })
                .collect()
        })
        .ok_or_else(|| format!("missing array '{key}'"))
}

fn region<'a>(body: &'a Value, id: &str) -> Result<&'a Value, String> {
    body["regions"]
        .as_array()
        .and_then(|regions| regions.iter().find(|region| region["id"] == id))
        .ok_or_else(|| format!("missing region {id}"))
}

#[tokio::test]
async fn production_router_exposes_region_location_discovery()
-> Result<(), Box<dyn std::error::Error>> {
    let app =
        production_router().map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    let (status, body) = get_json(app, "/o3k/v1/regions").await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"].as_u64().unwrap_or(0), 2);
    assert_eq!(string_list(&body, "regions")?, vec!["region-a", "region-b"]);
    assert_eq!(
        string_list(region(&body, "region-a")?, "availability_domains")?,
        vec!["az-1", "az-2"]
    );
    Ok(())
}

#[tokio::test]
async fn production_router_exposes_regional_placement_on_resource_types()
-> Result<(), Box<dyn std::error::Error>> {
    let app =
        production_router().map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    let (status, body) = get_json(app, "/o3k/v1/resource-types").await?;
    assert_eq!(status, StatusCode::OK);
    let resource = body["resource_types"]
        .as_array()
        .ok_or("missing resource_types")?
        .iter()
        .find(|resource| resource["service"] == "compute")
        .ok_or("missing compute resource")?;
    assert_eq!(resource["placement"], "regional");
    assert_eq!(
        string_list(resource, "regions")?,
        vec!["region-a", "region-b"]
    );
    assert_eq!(resource["availability_domain_selection"], "optional");
    // Production router must advertise the endpoint in its own API root.
    let root_router =
        production_router().map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    let (_, root) = get_json(root_router, "/o3k/v1").await?;
    assert!(
        root["endpoints"]
            .as_array()
            .ok_or("missing endpoints")?
            .iter()
            .any(|endpoint| endpoint == "/o3k/v1/regions"),
        "api_root must advertise /o3k/v1/regions"
    );
    Ok(())
}

#[tokio::test]
async fn production_router_without_locations_reports_none() -> Result<(), Box<dyn std::error::Error>>
{
    let mut registry = ManifestRegistry::new();
    registry.seed_core().map_err(|error| error.to_string())?;
    let native = o3k_native_api::NativeApiState::new(
        Some(registry),
        o3k_native_api::pagination::CursorConfig::default(),
        None,
        None,
        None,
        None,
    )?;
    let router = o3k_api::router_with_state(o3k_api::AppState::new().with_native_api(native));
    let (status, body) = get_json(router, "/o3k/v1/regions").await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"].as_u64().unwrap_or(1), 0);
    assert_eq!(
        body["regions"].as_array().ok_or("missing regions")?.len(),
        0
    );
    Ok(())
}
