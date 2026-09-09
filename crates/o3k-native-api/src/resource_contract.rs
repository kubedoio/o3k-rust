//! Public native resource contracts.  A contract is both the deserialization
//! boundary used by the native adapter and the source for discovery schema.
//! It deliberately contains API fields only; provider and persistence fields
//! do not belong here.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize, Serializer};
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
pub struct ComputeServerUpdateSpec {
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VolumeCreateSpec {
    #[schemars(range(min = 1))]
    pub size_bytes: u64,
    #[schemars(length(min = 1, max = 128))]
    pub volume_type: String,
    #[serde(default)]
    #[schemars(length(max = 256))]
    pub name: Option<String>,
    #[serde(default)]
    #[schemars(length(max = 4096))]
    pub description: Option<String>,
    #[serde(default)]
    pub metadata: Option<std::collections::BTreeMap<String, String>>,
    /// Dynamic location identity; valid values come from the #889 location
    /// discovery authority and are intentionally not copied into an enum.
    #[serde(default)]
    #[schemars(length(max = 128))]
    pub availability_zone: Option<String>,
}

const MAX_VOLUME_METADATA_ENTRIES: usize = 64;
const MAX_VOLUME_METADATA_KEY_BYTES: usize = 128;
const MAX_VOLUME_METADATA_VALUE_BYTES: usize = 1024;

/// Keys in caller-supplied volume metadata are part of the public contract,
/// not an escape hatch for controller/provider state.  Keep this deny-list in
/// the contract validator so a secret-bearing request is rejected before it
/// reaches persistence or execution (the read-side projection also redacts).
fn is_forbidden_metadata_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    [
        "password",
        "token",
        "secret",
        "credential",
        "privatekey",
        "userdata",
        "environment",
        "connection",
        "connectionstring",
        "providerid",
        "providerhost",
        "providerpath",
        "hostpath",
        "devicepath",
        "chapsecret",
        "sourcekey",
        "migrationid",
    ]
    .contains(&normalized.as_str())
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkCreateSpec {
    pub name: String,
}

/// Canonical image metadata accepted by the native image resource create
/// contract.  Artifact bytes are deliberately a separate UploadImage action;
/// create never accepts provider paths, credentials, or embedded payloads.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageCreateSpec {
    pub name: String,
    #[serde(default = "default_private_visibility")]
    pub visibility: String,
    pub container_format: String,
    pub disk_format: String,
}

fn default_private_visibility() -> String {
    "private".to_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractKind {
    ComputeServer,
    Volume,
    Network,
    Image,
}

/// A public spec accepted by the generic application extension point.
/// Native resources construct it through `validate`; external-controller
/// adapters may construct it only after applying their authoritative contract.
#[derive(Debug, Clone)]
pub struct ValidatedSpec {
    value: Value,
}

impl std::ops::Deref for ValidatedSpec {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl Serialize for ValidatedSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.value.serialize(serializer)
    }
}

impl ValidatedSpec {
    /// Construct a spec supplied by an already-authoritative external
    /// controller contract. Native built-in callers must use `ContractKind`.
    pub fn from_external_contract(value: Value) -> Self {
        Self { value }
    }

    pub fn into_value(self) -> Value {
        self.value
    }
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
            "image:image" => Some(Self::Image),
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
            Self::Image => generator.into_root_schema_for::<ImageCreateSpec>(),
        };
        let mut value = serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({}));
        // These are semantic restrictions enforced by `validate`, so expose
        // them during discovery as well.  Without these constraints a client
        // can successfully validate a request against discovery and then be
        // rejected by the runtime boundary.
        if matches!(self, Self::Image)
            && let Some(properties) = value
                .pointer_mut("/properties")
                .and_then(Value::as_object_mut)
        {
            properties.insert(
                "visibility".into(),
                serde_json::json!({"type":"string", "const":"private"}),
            );
            properties.insert(
                "container_format".into(),
                serde_json::json!({"type":"string", "const":"bare"}),
            );
            properties.insert(
                "disk_format".into(),
                serde_json::json!({"type":"string", "enum":["raw","qcow2"]}),
            );
        }
        // Keep discovery honest with the stricter runtime guard below.  The
        // derive can describe map value types, but cannot express the bounded
        // metadata collection contract by itself.
        if matches!(self, Self::Volume)
            && let Some(metadata) = value
                .pointer_mut("/properties/metadata")
                .and_then(Value::as_object_mut)
        {
            metadata.insert(
                "maxProperties".into(),
                Value::from(MAX_VOLUME_METADATA_ENTRIES),
            );
            metadata.insert(
                "propertyNames".into(),
                serde_json::json!({"type":"string", "minLength":1, "maxLength":MAX_VOLUME_METADATA_KEY_BYTES}),
            );
            if let Some(values) = metadata
                .get_mut("additionalProperties")
                .and_then(Value::as_object_mut)
            {
                values.insert(
                    "maxLength".into(),
                    Value::from(MAX_VOLUME_METADATA_VALUE_BYTES),
                );
            }
        }
        value
    }

    pub fn validate(&self, spec: Value) -> Result<ValidatedSpec, serde_json::Error> {
        match self {
            Self::ComputeServer => serde_json::from_value::<ComputeServerCreateSpec>(spec.clone())
                .map(|_| ValidatedSpec { value: spec }),
            Self::Volume => {
                serde_json::from_value::<VolumeCreateSpec>(spec.clone()).and_then(|parsed| {
                    if parsed.size_bytes == 0 || parsed.volume_type.trim().is_empty() {
                        return Err(serde::de::Error::custom(
                            "size_bytes and volume_type must be non-empty",
                        ));
                    }
                    if parsed.metadata.as_ref().is_some_and(|metadata| {
                        metadata.len() > MAX_VOLUME_METADATA_ENTRIES
                            || metadata.iter().any(|(key, value)| {
                                key.len() > MAX_VOLUME_METADATA_KEY_BYTES
                                    || value.len() > MAX_VOLUME_METADATA_VALUE_BYTES
                                    || is_forbidden_metadata_key(key)
                            })
                    }) {
                        return Err(serde::de::Error::custom(
                            "metadata contains a reserved or sensitive key",
                        ));
                    }
                    Ok(ValidatedSpec { value: spec })
                })
            }
            Self::Network => serde_json::from_value::<NetworkCreateSpec>(spec.clone())
                .map(|_| ValidatedSpec { value: spec }),
            Self::Image => {
                serde_json::from_value::<ImageCreateSpec>(spec.clone()).and_then(|parsed| {
                    if parsed.name.trim().is_empty()
                        || parsed.container_format.trim().is_empty()
                        || parsed.disk_format.trim().is_empty()
                        // The native image authority currently supports only
                        // private bare images in its create path. Do not
                        // advertise metadata the service will reject.
                        || parsed.visibility != "private"
                        || parsed.container_format != "bare"
                        || !matches!(parsed.disk_format.as_str(), "raw" | "qcow2")
                    {
                        return Err(serde::de::Error::custom("invalid image metadata"));
                    }
                    Ok(ValidatedSpec { value: spec })
                })
            }
        }
    }

    pub fn update_schema(&self) -> Value {
        let settings = schemars::generate::SchemaSettings::draft2020_12();
        let generator = settings.into_generator();
        let schema = match self {
            Self::ComputeServer => generator.into_root_schema_for::<ComputeServerUpdateSpec>(),
            _ => return serde_json::json!({}),
        };
        serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({}))
    }

    pub fn validate_update(&self, spec: Value) -> Result<ValidatedSpec, serde_json::Error> {
        match self {
            Self::ComputeServer => serde_json::from_value::<ComputeServerUpdateSpec>(spec.clone())
                .and_then(|parsed| {
                    if parsed.name.trim().is_empty() {
                        return Err(serde::de::Error::custom("name must be non-empty"));
                    }
                    Ok(ValidatedSpec { value: spec })
                }),
            _ => Err(serde::de::Error::custom("update contract is not declared")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn generated_contracts_are_resource_specific_and_validate_the_same_input()
    -> Result<(), Box<dyn std::error::Error>> {
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
            (
                ContractKind::Image,
                json!({"name":"base","container_format":"bare","disk_format":"qcow2"}),
            ),
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
        assert_ne!(schemas[2], schemas[3]);
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
        assert!(
            ContractKind::Image
                .validate(json!({
                    "name":"base", "container_format":"bare", "disk_format":"qcow2",
                    "visibility":"public"
                }))
                .is_err()
        );
        assert!(
            ContractKind::Volume
                .validate(json!({
                    "size_bytes": 1024,
                    "volume_type": "local",
                    "metadata": {"provider_path": "/dev/private"}
                }))
                .is_err()
        );
        assert!(
            ContractKind::Volume
                .validate(json!({
                    "size_bytes": 1024,
                    "volume_type": "local",
                    "metadata": {"k": "x".repeat(1025)}
                }))
                .is_err()
        );
        let too_many = (0..65)
            .map(|index| (format!("key-{index}"), "value"))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert!(
            ContractKind::Volume
                .validate(json!({
                    "size_bytes": 1024,
                    "volume_type": "local",
                    "metadata": too_many
                }))
                .is_err()
        );
        assert!(
            ContractKind::Image
                .validate(json!({
                    "name":"base", "container_format":"bare", "disk_format":"qcow2",
                    "provider_path":"/secret"
                }))
                .is_err()
        );
        // Discovery must reject the same image capability restrictions as
        // the runtime validator; otherwise generic clients receive a false
        // contract and cannot avoid a guaranteed 400 response.
        let image_schema = ContractKind::Image.schema();
        let image_validator = jsonschema::validator_for(&image_schema)?;
        assert!(image_validator
            .validate(&json!({"name":"base","container_format":"bare","disk_format":"qcow2","visibility":"public"}))
            .is_err());
        assert!(
            image_validator
                .validate(&json!({"name":"base","container_format":"ova","disk_format":"qcow2"}))
                .is_err()
        );
        assert!(
            image_validator
                .validate(&json!({"name":"base","container_format":"bare","disk_format":"vmdk"}))
                .is_err()
        );

        // Discovery must reject the same bounded metadata shapes as the
        // runtime validator, before a client submits a request.
        let volume_schema = ContractKind::Volume.schema();
        let validator = jsonschema::validator_for(&volume_schema)?;
        let too_many = (0..65)
            .map(|index| (format!("key-{index}"), json!("value")))
            .collect::<serde_json::Map<_, _>>();
        assert!(
            validator
                .validate(&json!({"size_bytes":1024,"volume_type":"local","metadata":too_many}))
                .is_err()
        );
        assert!(validator
            .validate(&json!({"size_bytes":1024,"volume_type":"local","metadata":{"k":"x".repeat(1025)}}))
            .is_err());
        Ok(())
    }

    #[test]
    fn resolution_fails_closed_for_unknown_versions_and_resources() {
        assert!(ContractKind::for_resource("compute:server", "v2").is_none());
        assert!(ContractKind::for_resource("network:router", "v1").is_none());
    }
}
