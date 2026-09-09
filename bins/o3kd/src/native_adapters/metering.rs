use std::sync::Arc;

use o3k_native_api::{
    error::NativeReadError,
    metering::{MeterDefinitionView, MeterReader, MeterUsageView, MeterView},
};
use o3k_store::{DurableStore, MeteringRepository, O3kStore};

const METER_KINDS: &[(&str, &str)] = &[
    ("compute:server_count", "compute:server"),
    ("image:image_count", "image:image"),
    ("volume:volume_count", "volume"),
    ("network:network_count", "network:network"),
    ("network:subnet_count", "network:subnet"),
    ("network:port_count", "network:port"),
];

// Historical usage is exposed only for transitions emitted by the durable
// resource repository. These are event counters, deliberately not telemetry,
// duration, pricing, or provider-derived usage.
const LIFECYCLE_METERS: &[(&str, &str)] = &[
    ("compute:server:lifecycle_created", "compute:server"),
    ("compute:server:lifecycle_deleted", "compute:server"),
    ("compute:server:lifecycle_revived", "compute:server"),
    ("image:image:lifecycle_created", "image:image"),
    ("image:image:lifecycle_deleted", "image:image"),
    ("image:image:lifecycle_revived", "image:image"),
    ("network:network:lifecycle_created", "network:network"),
    ("network:network:lifecycle_deleted", "network:network"),
    ("network:network:lifecycle_revived", "network:network"),
    ("network:subnet:lifecycle_created", "network:subnet"),
    ("network:subnet:lifecycle_deleted", "network:subnet"),
    ("network:subnet:lifecycle_revived", "network:subnet"),
    ("network:port:lifecycle_created", "network:port"),
    ("network:port:lifecycle_deleted", "network:port"),
    ("network:port:lifecycle_revived", "network:port"),
];

pub struct MeterReaderAdapter {
    pub store: Arc<O3kStore>,
}

#[async_trait::async_trait]
impl MeterReader for MeterReaderAdapter {
    async fn list_definitions(&self) -> Result<Vec<MeterDefinitionView>, NativeReadError> {
        let mut definitions: Vec<MeterDefinitionView> = METER_KINDS
            .iter()
            .map(|(name, kind)| MeterDefinitionView {
                id: (*name).to_owned(),
                owning_service: kind.split(':').next().unwrap_or("o3k").to_owned(),
                unit: "count",
                aggregation: "gauge",
                applicability: "project",
                supported_granularities: vec!["instant"],
                tenant_visible: true,
                status: "available",
            })
            .collect();
        definitions.extend(
            LIFECYCLE_METERS
                .iter()
                .map(|(name, kind)| MeterDefinitionView {
                    id: (*name).to_owned(),
                    owning_service: kind.split(':').next().unwrap_or("o3k").to_owned(),
                    unit: "event",
                    aggregation: "sum",
                    applicability: "project",
                    supported_granularities: vec!["bounded_interval"],
                    tenant_visible: true,
                    status: "available",
                }),
        );
        Ok(definitions)
    }

    async fn read_project(&self, project_id: &str) -> Result<Vec<MeterView>, NativeReadError> {
        let mut result = Vec::with_capacity(METER_KINDS.len());
        let as_of = chrono::Utc::now().to_rfc3339();
        for (name, kind) in METER_KINDS {
            let value = self
                .store
                .count_resources(project_id, kind)
                .await
                .map_err(|error| {
                    tracing::error!(%error, meter = *name, "native meter count failed");
                    NativeReadError::Internal
                })?;
            result.push(MeterView {
                name: (*name).to_owned(),
                unit: "count",
                value,
                complete: true,
                as_of: as_of.clone(),
                authority: "o3k-resource-lifecycle",
            });
        }
        Ok(result)
    }

    async fn read_usage(
        &self,
        project_id: &str,
        meter_id: &str,
        effective_from: &str,
        effective_to: &str,
        limit: usize,
    ) -> Result<MeterUsageView, NativeReadError> {
        // Keep the repository query behind the versioned, code-declared
        // meter catalogue.  Otherwise a caller could ask for an arbitrary
        // meter id and receive an apparently authoritative aggregate for a
        // private/future event stream that is not part of the native
        // contract.  Unknown meters are intentionally indistinguishable
        // from an unavailable resource at the HTTP boundary.
        // Snapshot gauges are intentionally not accepted by the historical
        // event aggregate.  They are computed from current resource state,
        // and no append-only events exist for their value; returning an empty
        // aggregate here would falsely turn an unavailable history into an
        // authoritative zero.  Only meters with an explicit durable event
        // authority may be queried through this endpoint.
        if !LIFECYCLE_METERS.iter().any(|(id, _)| *id == meter_id) {
            return Err(NativeReadError::NotFound);
        }
        // The repository performs the bounded aggregate in SQL. No resource
        // or event collection is materialized in the HTTP process.
        let aggregate = self
            .store
            .aggregate_metering_events(project_id, meter_id, effective_from, effective_to, limit)
            .await
            .map_err(|error| {
                tracing::error!(%error, %meter_id, "native metering aggregate failed");
                NativeReadError::Internal
            })?;
        Ok(MeterUsageView {
            meter_id: aggregate.meter_id,
            unit: aggregate.unit,
            total_quantity: aggregate.total_quantity,
            event_count: aggregate.event_count,
            effective_from: aggregate.effective_from,
            effective_to: aggregate.effective_to,
            complete: aggregate.complete,
            authority: "o3k-metering-events",
        })
    }
}
