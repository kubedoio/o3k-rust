//! Public native resource contracts.  A contract is both the deserialization
//! boundary used by the native adapter and the source for discovery schema.
//! It deliberately contains API fields only; provider and persistence fields
//! do not belong here.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComputeServerCreateSpec {
    pub name: String,
    pub image_id: String,
    #[schemars(with = "String")]
    pub flavor_id: uuid::Uuid,
    pub network_ids: Vec<String>,
    #[serde(default)]
    pub key_name: Option<String>,
    #[serde(default)]
    pub ssh_public_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VolumeCreateSpec {
    #[schemars(range(min = 1))]
    pub size_bytes: u64,
    pub volume_type: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub metadata: Option<std::collections::BTreeMap<String, String>>,
    /// Dynamic location identity; valid values come from the #889 location
    /// discovery authority and are intentionally not copied into an enum.
    #[serde(default)]
    pub availability_zone: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkCreateSpec {
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractKind {
    ComputeServer,
    Volume,
    Network,
}

impl ContractKind {
    pub fn for_resource(resource: &str, version: &str) -> Option<Self> {
        if version != "v1" {
            return None;
        }
        match resource {
            "compute:server" => Some(Self::ComputeServer),
            "volume:volume" => Some(Self::Volume),
            "network:network" => Some(Self::Network),
            _ => None,
        }
    }

    pub fn schema(&self) -> Value {
        // schemars 0.8 emits the repository's 2019-09-compatible vocabulary;
        // the composed public document is consumed as Draft 2020-12 (the
        // generated subset uses no draft-specific extensions).
        let settings = schemars::generate::SchemaSettings::draft2020_12();
        let generator = settings.into_generator();
        let schema = match self {
            Self::ComputeServer => generator.into_root_schema_for::<ComputeServerCreateSpec>(),
            Self::Volume => generator.into_root_schema_for::<VolumeCreateSpec>(),
            Self::Network => generator.into_root_schema_for::<NetworkCreateSpec>(),
        };
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({}))
    }

    pub fn validate(&self, spec: Value) -> Result<Value, serde_json::Error> {
        match self {
            Self::ComputeServer => {
                serde_json::from_value::<ComputeServerCreateSpec>(spec.clone()).map(|_| spec)
            }
            Self::Volume => {
                serde_json::from_value::<VolumeCreateSpec>(spec.clone()).and_then(|parsed| {
                    if parsed.size_bytes == 0 || parsed.volume_type.trim().is_empty() {
                        return Err(serde::de::Error::custom(
                            "size_bytes and volume_type must be non-empty",
                        ));
                    }
                    Ok(spec)
                })
            }
            Self::Network => {
                serde_json::from_value::<NetworkCreateSpec>(spec.clone()).map(|_| spec)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn generated_contracts_are_resource_specific_and_validate_the_same_input() {
        let cases = [
            (
                ContractKind::ComputeServer,
                json!({"name":"vm","image_id":"img","flavor_id":"00000000-0000-0000-0000-000000000001","network_ids":["net"]}),
            ),
            (
                ContractKind::Volume,
                json!({"size_bytes":1024,"volume_type":"local"}),
            ),
            (ContractKind::Network, json!({"name":"private"})),
        ];
        let schemas: Vec<_> = cases
            .iter()
            .map(|(kind, value)| {
                assert!(kind.validate(value.clone()).is_ok());
                kind.schema()
            })
            .collect();
        for (schema, (_, value)) in schemas.iter().zip(cases.iter()) {
            assert!(jsonschema::validator_for(schema).is_ok());
            assert_eq!(
                schema.get("$schema").and_then(Value::as_str),
                Some("https://json-schema.org/draft/2020-12/schema")
            );
            assert!(schema.get("properties").is_some());
            assert!(value.is_object());
        }
        assert_ne!(schemas[0], schemas[1]);
        assert_ne!(schemas[1], schemas[2]);
        assert!(
            ContractKind::Volume
                .validate(json!({"size_bytes":0,"volume_type":"local"}))
                .is_err()
        );
        assert!(
            ContractKind::ComputeServer
                .validate(json!({"name":"vm"}))
                .is_err()
        );
        assert!(ContractKind::Network.validate(json!({"name":3})).is_err());
    }

    #[test]
    fn resolution_fails_closed_for_unknown_versions_and_resources() {
        assert!(ContractKind::for_resource("compute:server", "v2").is_none());
        assert!(ContractKind::for_resource("network:router", "v1").is_none());
    }
}
