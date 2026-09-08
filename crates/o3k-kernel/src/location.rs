//! Canonical O3K location topology (regions and availability domains).
//!
//! This module owns the single authoritative representation of deployment
//! location topology. A [`LocationRegistry`] is seeded by the deployment
//! composition root from configuration — never derived from compute providers,
//! hosts, hypervisors, clusters, storage backends, or network controllers.
//!
//! Public location IDs are provider-neutral and stable: replacing an
//! implementation provider must not change a region's public identity.
//!
//! See ADR-0181 and SPEC-0038.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::manifest::ServiceManifest;

/// Identifier characters permitted in a canonical location ID.
///
/// Location IDs are restricted to a stable, human-safe, provider-neutral
/// alphabet: lower-case letters, digits, `_` and `-`. IDs must not contain
/// upper-case, whitespace, or punctuation that could collide with host,
/// provider, or backend identifiers.
fn is_location_id_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
}

/// Returns true when `id` is a valid canonical location identifier.
///
/// The accepted alphabet matches the public contract pattern
/// `^[a-z0-9][a-z0-9_-]*$` (1..=128): the first character must be a lower-case
/// letter or digit; the remainder may add `_` and `-`.
fn valid_location_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        None => false,
        Some(first) => {
            let first_ok = first.is_ascii_lowercase() || first.is_ascii_digit();
            first_ok && id.len() <= 128 && chars.all(is_location_id_char)
        }
    }
}

/// A canonical availability domain within a region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailabilityDomain {
    /// Stable provider-neutral availability domain identifier.
    pub id: String,
}

/// A canonical region and the availability domains it contains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionDeclaration {
    /// Stable provider-neutral region identifier.
    pub id: String,
    /// Availability domains that belong to this region.
    #[serde(default)]
    pub availability_domains: Vec<AvailabilityDomain>,
}

/// Errors produced while validating canonical location topology or service
/// location references.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LocationError {
    #[error("empty region id")]
    EmptyRegionId,
    #[error("empty availability domain id")]
    EmptyAvailabilityDomainId,
    #[error("malformed region id '{0}'")]
    MalformedRegionId(String),
    #[error("malformed availability domain id '{0}'")]
    MalformedAvailabilityDomainId(String),
    #[error("duplicate region: {0}")]
    DuplicateRegion(String),
    #[error("duplicate availability domain '{0}' in region '{1}'")]
    DuplicateAvailabilityDomainInRegion(String, String),
    #[error("ambiguous availability domain '{0}' declared in multiple regions")]
    AmbiguousAvailabilityDomain(String),
    #[error("service '{service}' declares unknown region '{region}'")]
    UnknownRegion { service: String, region: String },
    #[error("service '{service}' declares unknown availability domain '{availability_domain}'")]
    UnknownAvailabilityDomain {
        service: String,
        availability_domain: String,
    },
    #[error(
        "service '{service}' references availability domain '{availability_domain}' of region '{region}' it does not declare"
    )]
    AvailabilityDomainOutsideDeclaredRegions {
        service: String,
        availability_domain: String,
        region: String,
    },
}

/// The single canonical authority for O3K location topology.
///
/// A [`LocationRegistry`] is read-only after construction. It is built via
/// [`LocationRegistry::from_declarations`], which applies deterministic
/// validation, and exposes only sorted, provider-neutral topology.
///
/// This is the one authority for "which regions and availability domains
/// exist". Service manifests reference (filter) canonical IDs; they never
/// invent location identity.
///
/// Note: `LocationRegistry` intentionally does not implement `Deserialize` so
/// its sorted/validated invariants cannot be bypassed by deserializing an
/// arbitrary value. Use [`LocationRegistry::from_declarations`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LocationRegistry {
    regions: Vec<RegionDeclaration>,
}

impl LocationRegistry {
    /// Builds a validated, deterministically ordered registry.
    ///
    /// Validation rejects empty/malformed IDs, duplicate region IDs, duplicate
    /// availability-domain IDs within a region, and the same availability
    /// domain appearing in more than one region (an ambiguous mapping).
    pub fn from_declarations(declarations: Vec<RegionDeclaration>) -> Result<Self, LocationError> {
        let mut seen_regions = HashMap::new();
        // Tracks availability-domain -> region for cross-region duplicates.
        let mut az_to_region: HashMap<&str, &str> = HashMap::new();

        for region in &declarations {
            validate_region_id(region)?;
            if seen_regions.insert(&region.id, ()).is_some() {
                return Err(LocationError::DuplicateRegion(region.id.clone()));
            }
            let mut seen_az_in_region = HashMap::new();
            for az in &region.availability_domains {
                validate_az_id(az)?;
                if seen_az_in_region.insert(az.id.as_str(), ()).is_some() {
                    return Err(LocationError::DuplicateAvailabilityDomainInRegion(
                        az.id.clone(),
                        region.id.clone(),
                    ));
                }
                if az_to_region
                    .insert(az.id.as_str(), region.id.as_str())
                    .is_some()
                {
                    return Err(LocationError::AmbiguousAvailabilityDomain(az.id.clone()));
                }
            }
        }

        let mut registry = Self {
            regions: declarations
                .into_iter()
                .map(|mut region| {
                    region.availability_domains.sort_by(|a, b| a.id.cmp(&b.id));
                    region
                })
                .collect(),
        };
        registry.regions.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(registry)
    }

    /// Returns the regions in deterministic (sorted by id) order.
    #[must_use]
    pub fn regions(&self) -> &[RegionDeclaration] {
        &self.regions
    }

    /// Returns true if no regions are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// Returns the number of configured regions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    /// Returns true when the region identifier is a configured canonical region.
    #[must_use]
    pub fn contains_region(&self, id: &str) -> bool {
        self.regions.iter().any(|region| region.id == id)
    }

    /// Returns the canonical region with the given id, if configured.
    #[must_use]
    pub fn region(&self, id: &str) -> Option<&RegionDeclaration> {
        self.regions.iter().find(|region| region.id == id)
    }

    /// Returns the availability-domain identifiers declared for `region`.
    #[must_use]
    pub fn availability_domains_of(&self, region: &str) -> &[AvailabilityDomain] {
        self.region(region)
            .map(|region| region.availability_domains.as_slice())
            .unwrap_or(&[])
    }

    /// Validates that a service manifest references only canonical locations.
    ///
    /// A manifest's `regions` and `availability_domains` are *filters* over the
    /// canonical registry: every referenced id must already exist as canonical
    /// O3K location identity, and (when the manifest declares regional scope)
    /// every declared availability domain must belong to one of the regions the
    /// manifest itself advertises. This fails closed so a manifest can never
    /// invent location truth, drift from the registry, or silently depend on an
    /// availability domain whose region it does not advertise.
    pub fn validate_manifest_locations(
        &self,
        manifest: &ServiceManifest,
    ) -> Result<(), LocationError> {
        let az_index: HashMap<&str, &str> = self
            .regions
            .iter()
            .flat_map(|region| {
                region
                    .availability_domains
                    .iter()
                    .map(move |az| (az.id.as_str(), region.id.as_str()))
            })
            .collect();

        // The set of canonical regions this manifest explicitly advertises.
        // Empty means the manifest places globally (no regional restriction),
        // in which case any canonical AZ is acceptable.
        let declared_regions: std::collections::BTreeSet<&str> =
            manifest.regions.iter().map(String::as_str).collect();

        for region in &manifest.regions {
            if !self.contains_region(region) {
                return Err(LocationError::UnknownRegion {
                    service: manifest.service_id.clone(),
                    region: region.clone(),
                });
            }
        }
        for az in &manifest.availability_domains {
            match az_index.get(az.as_str()) {
                None => {
                    return Err(LocationError::UnknownAvailabilityDomain {
                        service: manifest.service_id.clone(),
                        availability_domain: az.clone(),
                    });
                }
                Some(owning_region)
                    if !declared_regions.is_empty()
                        && !declared_regions.contains(owning_region) =>
                {
                    return Err(LocationError::AvailabilityDomainOutsideDeclaredRegions {
                        service: manifest.service_id.clone(),
                        availability_domain: az.clone(),
                        region: owning_region.to_string(),
                    });
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    /// Validates every manifest in `manifests` against the canonical registry.
    ///
    /// Used by the composition root to fail closed at startup when any
    /// registered service references an unknown location.
    pub fn validate_manifest_registry(
        &self,
        registry: &crate::ManifestRegistry,
    ) -> Result<(), LocationError> {
        for manifest in registry.all() {
            self.validate_manifest_locations(manifest)?;
        }
        Ok(())
    }
}

fn validate_region_id(region: &RegionDeclaration) -> Result<(), LocationError> {
    if region.id.is_empty() {
        return Err(LocationError::EmptyRegionId);
    }
    if !valid_location_id(&region.id) {
        return Err(LocationError::MalformedRegionId(region.id.clone()));
    }
    Ok(())
}

fn validate_az_id(az: &AvailabilityDomain) -> Result<(), LocationError> {
    if az.id.is_empty() {
        return Err(LocationError::EmptyAvailabilityDomainId);
    }
    if !valid_location_id(&az.id) {
        return Err(LocationError::MalformedAvailabilityDomainId(az.id.clone()));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn declaration(id: &str, azs: &[&str]) -> RegionDeclaration {
        RegionDeclaration {
            id: id.to_owned(),
            availability_domains: azs
                .iter()
                .map(|az| AvailabilityDomain { id: az.to_string() })
                .collect(),
        }
    }

    fn registry(declarations: Vec<RegionDeclaration>) -> LocationRegistry {
        LocationRegistry::from_declarations(declarations).expect("valid declarations")
    }

    #[test]
    fn rejects_empty_region_id() {
        let err = LocationRegistry::from_declarations(vec![declaration("", &[])]).unwrap_err();
        assert_eq!(err, LocationError::EmptyRegionId);
    }

    #[test]
    fn rejects_empty_availability_domain_id() {
        let err =
            LocationRegistry::from_declarations(vec![declaration("region-a", &[""])]).unwrap_err();
        assert_eq!(err, LocationError::EmptyAvailabilityDomainId);
    }

    #[test]
    fn rejects_duplicate_region_ids() {
        let err = LocationRegistry::from_declarations(vec![
            declaration("region-a", &[]),
            declaration("region-a", &[]),
        ])
        .unwrap_err();
        assert_eq!(err, LocationError::DuplicateRegion("region-a".to_owned()));
    }

    #[test]
    fn rejects_duplicate_az_in_region() {
        let err =
            LocationRegistry::from_declarations(vec![declaration("region-a", &["az-1", "az-1"])])
                .unwrap_err();
        assert_eq!(
            err,
            LocationError::DuplicateAvailabilityDomainInRegion(
                "az-1".to_owned(),
                "region-a".to_owned()
            )
        );
    }

    #[test]
    fn rejects_az_across_two_regions() {
        let err = LocationRegistry::from_declarations(vec![
            declaration("region-a", &["az-1"]),
            declaration("region-b", &["az-1"]),
        ])
        .unwrap_err();
        assert_eq!(
            err,
            LocationError::AmbiguousAvailabilityDomain("az-1".to_owned())
        );
    }

    #[test]
    fn rejects_malformed_region_id() {
        let err = LocationRegistry::from_declarations(vec![declaration("Two Regions!", &[])])
            .unwrap_err();
        assert_eq!(
            err,
            LocationError::MalformedRegionId("Two Regions!".to_owned())
        );
    }

    #[test]
    fn rejects_region_id_with_leading_punctuation() {
        // Matches the public contract pattern ^[a-z0-9]... : a leading `-` or
        // `_` is invalid even though those characters are legal in the rest.
        for id in ["-east", "_east"] {
            let err = LocationRegistry::from_declarations(vec![declaration(id, &[])]).unwrap_err();
            assert_eq!(err, LocationError::MalformedRegionId(id.to_owned()));
        }
    }

    #[test]
    fn rejects_malformed_az_id_with_uppercase() {
        let err = LocationRegistry::from_declarations(vec![declaration("region-a", &["AZ-1"])])
            .unwrap_err();
        assert_eq!(
            err,
            LocationError::MalformedAvailabilityDomainId("AZ-1".to_owned())
        );
    }

    #[test]
    fn sorted_deterministically() {
        let registry = registry(vec![
            declaration("region-b", &["az-2", "az-1"]),
            declaration("region-a", &["az-3"]),
        ]);
        let ids: Vec<&str> = registry.regions().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["region-a", "region-b"]);
        let azs: Vec<&str> = registry
            .availability_domains_of("region-b")
            .iter()
            .map(|az| az.id.as_str())
            .collect();
        assert_eq!(azs, vec!["az-1", "az-2"]);
    }

    #[test]
    fn region_with_multiple_availability_domains() {
        let registry = registry(vec![declaration("region-a", &["az-1", "az-2", "az-3"])]);
        assert_eq!(registry.regions().len(), 1);
        assert_eq!(registry.availability_domains_of("region-a").len(), 3);
    }

    #[test]
    fn validate_manifest_rejects_unknown_region() {
        let registry = registry(vec![declaration("region-a", &["az-1"])]);
        let mut manifest = crate::ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "1".to_owned(),
            ownership: crate::ServiceOwnership::O3kImplemented,
            resource_types: vec![],
            actions: vec!["compute:ListServers".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec!["not-a-region".to_owned()],
            availability_domains: vec![],
            controller: None,
            health: None,
        };
        // A manifest must still carry at least the structural fields validate()
        // would require; here we only exercise location reference checking.
        let err = registry.validate_manifest_locations(&manifest).unwrap_err();
        assert_eq!(
            err,
            LocationError::UnknownRegion {
                service: "compute".to_owned(),
                region: "not-a-region".to_owned()
            }
        );
        manifest.regions = vec!["region-a".to_owned()];
        assert!(registry.validate_manifest_locations(&manifest).is_ok());
    }

    #[test]
    fn validate_manifest_rejects_unknown_availability_domain() {
        let registry = registry(vec![declaration("region-a", &["az-1"])]);
        let manifest = crate::ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "1".to_owned(),
            ownership: crate::ServiceOwnership::O3kImplemented,
            resource_types: vec![],
            actions: vec!["compute:ListServers".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec!["missing-az".to_owned()],
            controller: None,
            health: None,
        };
        let err = registry.validate_manifest_locations(&manifest).unwrap_err();
        assert_eq!(
            err,
            LocationError::UnknownAvailabilityDomain {
                service: "compute".to_owned(),
                availability_domain: "missing-az".to_owned()
            }
        );
    }

    #[test]
    fn validate_manifest_rejects_az_outside_declared_regions() {
        let registry = LocationRegistry::from_declarations(vec![
            declaration("region-a", &["az-a"]),
            declaration("region-b", &["az-b"]),
        ])
        .unwrap();
        // AZ `az-b` is canonical but belongs to region-b; a manifest that only
        // declares region-a must not silently depend on it.
        let manifest = crate::ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "1".to_owned(),
            ownership: crate::ServiceOwnership::O3kImplemented,
            resource_types: vec![],
            actions: vec!["compute:ListServers".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec!["region-a".to_owned()],
            availability_domains: vec!["az-b".to_owned()],
            controller: None,
            health: None,
        };
        let err = registry.validate_manifest_locations(&manifest).unwrap_err();
        assert_eq!(
            err,
            LocationError::AvailabilityDomainOutsideDeclaredRegions {
                service: "compute".to_owned(),
                availability_domain: "az-b".to_owned(),
                region: "region-b".to_owned()
            }
        );
        // The same AZ inside the declared region is valid.
        let ok_manifest = crate::ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "1".to_owned(),
            ownership: crate::ServiceOwnership::O3kImplemented,
            resource_types: vec![],
            actions: vec!["compute:ListServers".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec!["region-a".to_owned()],
            availability_domains: vec!["az-a".to_owned()],
            controller: None,
            health: None,
        };
        assert!(registry.validate_manifest_locations(&ok_manifest).is_ok());
    }

    #[test]
    fn region_identity_is_provider_independent_and_structurally_closed() {
        // Public region identity is purely the declared canonical id: providers
        // do not participate in location identity at all. Two configurations
        // that share region/AZ ids expose identical public topology regardless
        // of declaration order (which is how a hypothetical provider change
        // would otherwise surface).
        let a = registry(vec![
            declaration("region-a", &["az-1"]),
            declaration("region-b", &[]),
        ]);
        let b = registry(vec![
            declaration("region-b", &[]),
            declaration("region-a", &["az-1"]),
        ]);
        assert_eq!(a.regions(), b.regions());
        let serialized_a: serde_json::Value = serde_json::to_value(&a).unwrap();
        let serialized_b: serde_json::Value = serde_json::to_value(&b).unwrap();
        assert_eq!(serialized_a, serialized_b);

        // Structural closure: the serialized public topology carries exactly the
        // location-identity keys and nothing else, so provider/host/backend
        // identity cannot leak into tenant-facing location data.
        for region in serialized_a
            .get("regions")
            .and_then(serde_json::Value::as_array)
            .unwrap()
        {
            let mut keys: Vec<&str> = region
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort();
            assert_eq!(keys, vec!["availability_domains", "id"]);
            for az in region
                .get("availability_domains")
                .and_then(serde_json::Value::as_array)
                .unwrap()
            {
                let mut az_keys: Vec<&str> =
                    az.as_object().unwrap().keys().map(String::as_str).collect();
                az_keys.sort();
                assert_eq!(az_keys, vec!["id"]);
            }
        }
    }

    #[test]
    fn validate_manifest_registry_rejects_unknown_region_across_manifests() {
        use crate::manifest::{ManifestController, RegisteredResourceType, ResourceScope};
        use crate::resource::ResourceType;
        use crate::{ManifestRegistry, ServiceOwnership};
        // Two manifests: one references only canonical locations, the other
        // references an unknown region. The registry-level validator must fail
        // closed on the bad one.
        let registry = registry(vec![declaration("region-a", &["az-1"])]);
        let mut manifest_registry = ManifestRegistry::new();
        let controller = Some(ManifestController {
            mode: "in-process".to_owned(),
            protocol: "in-process".to_owned(),
            protocol_version: "1.0".to_owned(),
            service_principal: None,
        });
        let good = crate::ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "1".to_owned(),
            ownership: ServiceOwnership::O3kImplemented,
            resource_types: vec![RegisteredResourceType {
                resource_type: ResourceType::new_unchecked("compute", "server"),
                schema_version: "v1".to_owned(),
                collection: None,
                scope: ResourceScope::Tenant,
                operations: std::collections::HashMap::new(),
            }],
            actions: vec!["compute:ListServers".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec!["region-a".to_owned()],
            availability_domains: vec!["az-1".to_owned()],
            controller: controller.clone(),
            health: None,
        };
        let bad = crate::ServiceManifest {
            manifest_version: 1,
            service_id: "network".to_owned(),
            namespace: "network".to_owned(),
            service_version: "1".to_owned(),
            ownership: ServiceOwnership::O3kImplemented,
            resource_types: vec![RegisteredResourceType {
                resource_type: ResourceType::new_unchecked("network", "address_realm"),
                schema_version: "v1".to_owned(),
                collection: Some("address-realms".to_owned()),
                scope: ResourceScope::Tenant,
                operations: std::collections::HashMap::new(),
            }],
            actions: vec!["network:ListAddressRealms".to_owned()],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec!["region-b".to_owned()],
            availability_domains: vec![],
            controller,
            health: None,
        };
        manifest_registry.register(good).unwrap();
        manifest_registry.register(bad).unwrap();
        let err = registry
            .validate_manifest_registry(&manifest_registry)
            .unwrap_err();
        assert_eq!(
            err,
            LocationError::UnknownRegion {
                service: "network".to_owned(),
                region: "region-b".to_owned()
            }
        );
    }
}
