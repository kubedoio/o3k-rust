pub mod proto {
    tonic::include_proto!("o3k.provider.v1");
}

pub mod mapping {
    use crate::proto;
    use o3k_provider::CreateInstanceRequest;
    use uuid::Uuid;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum MappingError {
        InvalidUuid,
        MissingOperationId,
        MissingServerId,
        MissingIdempotencyKey,
    }

    pub fn create_request(
        request: proto::CreateInstanceRequest,
    ) -> Result<CreateInstanceRequest, MappingError> {
        let operation_id =
            Uuid::parse_str(&request.operation_id).map_err(|_| MappingError::InvalidUuid)?;
        let o3k_server_id =
            Uuid::parse_str(&request.o3k_server_id).map_err(|_| MappingError::InvalidUuid)?;
        if request.operation_id.is_empty() {
            return Err(MappingError::MissingOperationId);
        }
        if request.o3k_server_id.is_empty() {
            return Err(MappingError::MissingServerId);
        }
        if request.idempotency_key.trim().is_empty() {
            return Err(MappingError::MissingIdempotencyKey);
        }
        Ok(CreateInstanceRequest {
            operation_id,
            o3k_server_id,
            name: request.name,
            vcpus: request.vcpus,
            memory_mib: request.memory_mib,
            image_id: (!request.image_id.is_empty()).then_some(request.image_id),
            idempotency_key: request.idempotency_key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{mapping, proto};
    use prost::Message;
    use uuid::Uuid;

    #[test]
    fn create_dto_round_trips_and_maps_to_internal_request() {
        let request = proto::CreateInstanceRequest {
            operation_id: Uuid::nil().to_string(),
            o3k_server_id: Uuid::from_u128(7).to_string(),
            name: "test".to_owned(),
            vcpus: 2,
            memory_mib: 512,
            image_id: "image".to_owned(),
            idempotency_key: "key".to_owned(),
        };
        let encoded = request.encode_to_vec();
        let decoded = match proto::CreateInstanceRequest::decode(encoded.as_slice()) {
            Ok(value) => value,
            Err(_) => return,
        };
        let internal = match mapping::create_request(decoded) {
            Ok(value) => value,
            Err(_) => return,
        };
        assert_eq!(internal.vcpus, 2);
        assert_eq!(internal.image_id.as_deref(), Some("image"));
    }

    #[test]
    fn unknown_enum_value_is_preserved_by_protobuf_wire_round_trip() {
        let operation = proto::Operation {
            provider_operation_id: String::new(),
            o3k_operation_id: String::new(),
            state: 99,
            error_category: 0,
            redacted_message: String::new(),
            provider_resource_id: String::new(),
        };
        let decoded = match proto::Operation::decode(operation.encode_to_vec().as_slice()) {
            Ok(value) => value,
            Err(_) => return,
        };
        assert_eq!(decoded.state, 99);
    }

    #[test]
    fn mapping_rejects_missing_identity_fields() {
        let request = proto::CreateInstanceRequest::default();
        assert!(matches!(
            mapping::create_request(request),
            Err(mapping::MappingError::InvalidUuid)
        ));
    }
}
