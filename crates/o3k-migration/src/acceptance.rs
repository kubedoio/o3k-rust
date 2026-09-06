//! Fail-closed control flow for the twenty-gate P14.9 acceptance journey.
//!
//! This module is deliberately an evidence coordinator, not a second
//! migration engine.  [`AcceptanceDriver`] is the boundary where real source,
//! canonical destination, guest, packet-path, inventory, and toolchain
//! adapters are composed.  The coordinator owns only gate ordering, durable
//! run metadata, failure propagation, and evidence invariants.  A driver must
//! return observed proof for a gate; the coordinator never promotes a missing
//! observation to PASS.

use crate::runner::MigrationRunner;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, path::Path};
use thiserror::Error;

const PROFILE: &str = "p14-openstack-cold-migration-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(rename = "G01")]
pub enum GateId {
    G01,
    G02,
    G03,
    G04,
    G05,
    G06,
    G07,
    G08,
    G09,
    G10,
    G11,
    G12,
    G13,
    G14,
    G15,
    G16,
    G17,
    G18,
    G19,
    G20,
}

impl GateId {
    pub const ALL: [Self; 20] = [
        Self::G01,
        Self::G02,
        Self::G03,
        Self::G04,
        Self::G05,
        Self::G06,
        Self::G07,
        Self::G08,
        Self::G09,
        Self::G10,
        Self::G11,
        Self::G12,
        Self::G13,
        Self::G14,
        Self::G15,
        Self::G16,
        Self::G17,
        Self::G18,
        Self::G19,
        Self::G20,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::G01 => "G01",
            Self::G02 => "G02",
            Self::G03 => "G03",
            Self::G04 => "G04",
            Self::G05 => "G05",
            Self::G06 => "G06",
            Self::G07 => "G07",
            Self::G08 => "G08",
            Self::G09 => "G09",
            Self::G10 => "G10",
            Self::G11 => "G11",
            Self::G12 => "G12",
            Self::G13 => "G13",
            Self::G14 => "G14",
            Self::G15 => "G15",
            Self::G16 => "G16",
            Self::G17 => "G17",
            Self::G18 => "G18",
            Self::G19 => "G19",
            Self::G20 => "G20",
        }
    }
}

impl fmt::Display for GateId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GateStatus {
    NotRun,
    Blocked,
    Pass,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FailureClass {
    Environment,
    Implementation,
    Evidence,
    ExternalService,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateContext {
    pub run_id: String,
    pub migration_id: String,
    pub source_fingerprint: String,
    pub destination_fingerprint: String,
    pub tested_runtime_head_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateProof {
    pub evidence_refs: Vec<String>,
    pub source_bound: bool,
    pub destination_bound: bool,
    pub details: BTreeMap<String, String>,
}

impl GateProof {
    fn is_complete(&self) -> bool {
        self.source_bound
            && self.destination_bound
            && !self.evidence_refs.is_empty()
            && !self.details.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessReport {
    pub ready: bool,
    pub checks: BTreeMap<String, bool>,
    pub evidence_refs: Vec<String>,
    pub toolchain: BTreeMap<String, String>,
    pub diagnostic: String,
}

#[derive(Debug, Error)]
pub enum AcceptanceFailure {
    #[error("{class:?}: {message}")]
    Driver {
        class: FailureClass,
        message: String,
    },
    #[error("acceptance invariant failed: {0}")]
    Invariant(String),
    #[error("evidence persistence failed: {0}")]
    Persistence(String),
}

#[async_trait]
pub trait AcceptanceDriver: Send {
    /// Actively probes the configured source, destination, database, guest
    /// capability, packet path, and pinned toolchain. Boolean environment
    /// assertions are not a readiness result.
    async fn readiness(&mut self) -> Result<ReadinessReport, AcceptanceFailure>;

    /// Execute exactly one gate through real adapters. The driver may reuse
    /// `MigrationRunner` for migration semantics, but must not implement a
    /// competing migration state machine.
    async fn execute_gate(
        &mut self,
        gate: GateId,
        context: &GateContext,
    ) -> Result<GateProof, AcceptanceFailure>;

    /// Cleanup is explicit and ownership-aware. Foreign/sentinel state must
    /// never be selected by this operation.
    async fn cleanup(&mut self, context: &GateContext) -> Result<GateProof, AcceptanceFailure>;
}

/// Composition handle for a real P14 adapter. It makes the ownership rule
/// explicit: the acceptance driver receives the existing durable runner and
/// adds observation/evidence probes around it; it cannot replace the runner
/// with a shell or a second migration state machine.
pub struct MigrationRunnerComposition<S, D> {
    pub runner: MigrationRunner<S, D>,
}

impl<S, D> MigrationRunnerComposition<S, D> {
    #[must_use]
    pub const fn new(runner: MigrationRunner<S, D>) -> Self {
        Self { runner }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateRecord {
    pub gate: GateId,
    pub status: GateStatus,
    pub started_at: String,
    pub ended_at: String,
    pub migration_id: String,
    pub tested_runtime_head_sha: String,
    pub source_fingerprint: String,
    pub destination_fingerprint: String,
    pub evidence_refs: Vec<String>,
    pub failure_class: Option<FailureClass>,
    pub diagnostic: String,
    pub proof: Option<GateProof>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceSummary {
    pub owned_leaks: u64,
    pub inconsistencies: u64,
    pub foreign_state_changes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceEvidence {
    pub artifact_type: String,
    pub schema_version: u32,
    pub phase: String,
    pub profile: String,
    pub tested_runtime_head_sha: String,
    pub run_id: String,
    pub source: IdentityBinding,
    pub destination: IdentityBinding,
    pub environment_ready: bool,
    pub readiness: ReadinessReport,
    pub toolchain: BTreeMap<String, String>,
    pub execution_result: String,
    pub gates: Vec<GateRecord>,
    pub summary: AcceptanceSummary,
    pub opentofu: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityBinding {
    pub profile: String,
    pub fingerprint: String,
}

impl AcceptanceEvidence {
    #[must_use]
    pub fn blocked(context: &GateContext, diagnostic: impl Into<String>) -> Self {
        let now = timestamp();
        let diagnostic = diagnostic.into();
        Self {
            artifact_type: "o3k-p14-9a-evidence".into(),
            schema_version: 1,
            phase: "P14.9B".into(),
            profile: PROFILE.into(),
            tested_runtime_head_sha: context.tested_runtime_head_sha.clone(),
            run_id: context.run_id.clone(),
            source: IdentityBinding {
                profile: "openstack-source".into(),
                fingerprint: context.source_fingerprint.clone(),
            },
            destination: IdentityBinding {
                profile: "native-rust-testlab".into(),
                fingerprint: context.destination_fingerprint.clone(),
            },
            environment_ready: false,
            readiness: ReadinessReport {
                ready: false,
                checks: BTreeMap::new(),
                evidence_refs: vec!["readiness:blocked".into()],
                toolchain: BTreeMap::new(),
                diagnostic: diagnostic.clone(),
            },
            toolchain: BTreeMap::new(),
            execution_result: "blocked".into(),
            gates: GateId::ALL
                .into_iter()
                .map(|gate| GateRecord {
                    gate,
                    status: GateStatus::Blocked,
                    started_at: now.clone(),
                    ended_at: now.clone(),
                    migration_id: context.migration_id.clone(),
                    tested_runtime_head_sha: context.tested_runtime_head_sha.clone(),
                    source_fingerprint: context.source_fingerprint.clone(),
                    destination_fingerprint: context.destination_fingerprint.clone(),
                    evidence_refs: vec![format!("readiness:{gate}")],
                    failure_class: Some(FailureClass::Environment),
                    diagnostic: diagnostic.clone(),
                    proof: None,
                })
                .collect(),
            summary: AcceptanceSummary {
                owned_leaks: 0,
                inconsistencies: 0,
                foreign_state_changes: 0,
            },
            opentofu: BTreeMap::from([(String::from("final_plan"), String::from("not-run"))]),
        }
    }

    #[must_use]
    pub fn readiness_only(context: &GateContext, readiness: ReadinessReport) -> Self {
        let mut evidence = Self::blocked(context, readiness.diagnostic.clone());
        evidence.readiness = readiness.clone();
        evidence.toolchain = readiness.toolchain;
        evidence
    }

    pub fn validate(&self) -> Result<(), AcceptanceFailure> {
        if self.artifact_type != "o3k-p14-9a-evidence"
            || self.schema_version != 1
            || self.profile != PROFILE
            || self.source.fingerprint.is_empty()
            || self.destination.fingerprint.is_empty()
            || self.tested_runtime_head_sha.len() != 40
            || !self
                .tested_runtime_head_sha
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(AcceptanceFailure::Invariant(
                "artifact identity is invalid".into(),
            ));
        }
        if self
            .gates
            .iter()
            .map(|record| record.gate)
            .collect::<Vec<_>>()
            != GateId::ALL
        {
            return Err(AcceptanceFailure::Invariant(
                "gate set/order is not exactly G01..G20".into(),
            ));
        }
        for record in &self.gates {
            if record.tested_runtime_head_sha != self.tested_runtime_head_sha
                || record.source_fingerprint != self.source.fingerprint
                || record.destination_fingerprint != self.destination.fingerprint
            {
                return Err(AcceptanceFailure::Invariant(format!(
                    "{} is not bound to the artifact identity",
                    record.gate
                )));
            }
            if record.status == GateStatus::Pass {
                let proof = record.proof.as_ref().ok_or_else(|| {
                    AcceptanceFailure::Invariant(format!("{} has no proof", record.gate))
                })?;
                if !proof.is_complete() {
                    return Err(AcceptanceFailure::Invariant(format!(
                        "{} has incomplete proof",
                        record.gate
                    )));
                }
            }
        }
        if self.execution_result == "passed"
            && (self.toolchain.get("opentofu").map(String::as_str) != Some("1.12.6")
                || self.toolchain.get("provider").map(String::as_str)
                    != Some("terraform-provider-openstack/openstack 3.4.0")
                || self.toolchain.get("provider_modified").map(String::as_str) != Some("false"))
        {
            return Err(AcceptanceFailure::Invariant(
                "PASS evidence is missing the pinned toolchain identity".into(),
            ));
        }
        if self.execution_result == "passed"
            && (!self.environment_ready
                || !self.readiness.ready
                || self
                    .gates
                    .iter()
                    .any(|record| record.status != GateStatus::Pass)
                || self.summary
                    != (AcceptanceSummary {
                        owned_leaks: 0,
                        inconsistencies: 0,
                        foreign_state_changes: 0,
                    })
                || self.opentofu.get("final_plan").map(String::as_str) != Some("NO-OP")
                || self.gates[17]
                    .proof
                    .as_ref()
                    .and_then(|proof| proof.details.get("final_plan"))
                    .map(String::as_str)
                    != Some("NO-OP"))
        {
            return Err(AcceptanceFailure::Invariant(
                "PASS evidence is incomplete".into(),
            ));
        }
        if self.execution_result == "passed"
            && self.gates[18]
                .proof
                .as_ref()
                .and_then(|proof| proof.details.get("project_b_unchanged"))
                .map(String::as_str)
                != Some("true")
        {
            return Err(AcceptanceFailure::Invariant(
                "PASS evidence is missing Project B isolation proof".into(),
            ));
        }
        Ok(())
    }

    pub fn write_json(&self, path: &Path) -> Result<(), AcceptanceFailure> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| AcceptanceFailure::Persistence(error.to_string()))?;
        std::fs::write(path, [bytes.as_slice(), b"\n"].concat())
            .map_err(|error| AcceptanceFailure::Persistence(error.to_string()))
    }
}

pub struct AcceptanceHarness<D> {
    driver: D,
    context: GateContext,
}

impl<D> AcceptanceHarness<D>
where
    D: AcceptanceDriver,
{
    #[must_use]
    pub fn new(driver: D, context: GateContext) -> Self {
        Self { driver, context }
    }

    pub async fn run(mut self) -> Result<AcceptanceEvidence, AcceptanceFailure> {
        let readiness = self.driver.readiness().await?;
        if !readiness.ready {
            return Ok(AcceptanceEvidence::blocked(
                &self.context,
                readiness.diagnostic,
            ));
        }

        let readiness_for_artifact = readiness.clone();
        let toolchain = readiness_for_artifact.toolchain.clone();
        let mut records = Vec::with_capacity(GateId::ALL.len());
        let mut failed = false;
        for gate in GateId::ALL {
            if failed {
                records.push(not_run_record(&self.context, gate));
                continue;
            }
            let started_at = timestamp();
            match self.driver.execute_gate(gate, &self.context).await {
                Ok(proof)
                    if proof.is_complete()
                        && gate_specific_proof_is_valid(gate, &proof, &self.context) =>
                {
                    records.push(GateRecord {
                        gate,
                        status: GateStatus::Pass,
                        started_at,
                        ended_at: timestamp(),
                        migration_id: self.context.migration_id.clone(),
                        tested_runtime_head_sha: self.context.tested_runtime_head_sha.clone(),
                        source_fingerprint: self.context.source_fingerprint.clone(),
                        destination_fingerprint: self.context.destination_fingerprint.clone(),
                        evidence_refs: proof.evidence_refs.clone(),
                        failure_class: None,
                        diagnostic: "observed proof recorded".into(),
                        proof: Some(proof),
                    })
                }
                Ok(_) => {
                    failed = true;
                    records.push(failed_record(
                        &self.context,
                        gate,
                        started_at,
                        FailureClass::Evidence,
                        "driver returned incomplete or gate-inconsistent proof",
                    ));
                }
                Err(error) => {
                    failed = true;
                    let class = match &error {
                        AcceptanceFailure::Driver { class, .. } => *class,
                        AcceptanceFailure::Invariant(_) => FailureClass::Evidence,
                        AcceptanceFailure::Persistence(_) => FailureClass::Evidence,
                    };
                    records.push(failed_record(
                        &self.context,
                        gate,
                        started_at,
                        class,
                        error.to_string(),
                    ));
                }
            }
        }

        let mut cleanup_failed = None;
        let summary = if failed {
            AcceptanceSummary {
                owned_leaks: 0,
                inconsistencies: 0,
                foreign_state_changes: 0,
            }
        } else {
            let cleanup = match self.driver.cleanup(&self.context).await {
                Ok(cleanup) => cleanup,
                Err(error) => {
                    cleanup_failed = Some(error.to_string());
                    GateProof {
                        evidence_refs: vec!["cleanup:failure".into()],
                        source_bound: true,
                        destination_bound: true,
                        details: BTreeMap::new(),
                    }
                }
            };
            let owned_leaks = cleanup
                .details
                .get("owned_leaks")
                .and_then(|value| value.parse().ok())
                .unwrap_or(1);
            let inconsistencies = cleanup
                .details
                .get("inconsistencies")
                .and_then(|value| value.parse().ok())
                .unwrap_or(1);
            let foreign_state_changes = cleanup
                .details
                .get("foreign_state_changes")
                .and_then(|value| value.parse().ok())
                .unwrap_or(1);
            AcceptanceSummary {
                owned_leaks,
                inconsistencies,
                foreign_state_changes,
            }
        };
        if let Some(diagnostic) = cleanup_failed {
            failed = true;
            if let Some(record) = records.last_mut() {
                record.status = GateStatus::Fail;
                record.failure_class = Some(FailureClass::Evidence);
                record.diagnostic = diagnostic;
                record.proof = None;
            }
        }
        let execution_result = if failed { "failed" } else { "passed" };
        let opentofu = records
            .get(17)
            .and_then(|record| record.proof.as_ref())
            .and_then(|proof| proof.details.get("final_plan").cloned())
            .map(|plan| BTreeMap::from([(String::from("final_plan"), plan)]))
            .unwrap_or_default();
        let evidence = AcceptanceEvidence {
            artifact_type: "o3k-p14-9a-evidence".into(),
            schema_version: 1,
            phase: "P14.9B".into(),
            profile: PROFILE.into(),
            tested_runtime_head_sha: self.context.tested_runtime_head_sha.clone(),
            run_id: self.context.run_id.clone(),
            source: IdentityBinding {
                profile: "openstack-source".into(),
                fingerprint: self.context.source_fingerprint.clone(),
            },
            destination: IdentityBinding {
                profile: "native-rust-testlab".into(),
                fingerprint: self.context.destination_fingerprint.clone(),
            },
            environment_ready: true,
            readiness: readiness_for_artifact,
            toolchain,
            execution_result: execution_result.into(),
            gates: records,
            summary,
            opentofu,
        };
        evidence.validate()?;
        Ok(evidence)
    }
}

fn gate_specific_proof_is_valid(gate: GateId, proof: &GateProof, context: &GateContext) -> bool {
    match gate {
        GateId::G09 => proof.details.get("rollback_migration_id") == Some(&context.migration_id),
        GateId::G10 => proof
            .details
            .get("fresh_migration_id")
            .is_some_and(|id| !id.is_empty() && id != &context.migration_id),
        GateId::G18 => proof.details.get("final_plan").map(String::as_str) == Some("NO-OP"),
        GateId::G20 => ["owned_leaks", "inconsistencies", "foreign_state_changes"]
            .into_iter()
            .all(|key| {
                proof
                    .details
                    .get(key)
                    .and_then(|value| value.parse::<u64>().ok())
                    == Some(0)
            }),
        _ => true,
    }
}

fn timestamp() -> String {
    Utc::now().to_rfc3339()
}

fn not_run_record(context: &GateContext, gate: GateId) -> GateRecord {
    let now = timestamp();
    GateRecord {
        gate,
        status: GateStatus::NotRun,
        started_at: now.clone(),
        ended_at: now,
        migration_id: context.migration_id.clone(),
        tested_runtime_head_sha: context.tested_runtime_head_sha.clone(),
        source_fingerprint: context.source_fingerprint.clone(),
        destination_fingerprint: context.destination_fingerprint.clone(),
        evidence_refs: vec![format!("not-run:{gate}")],
        failure_class: None,
        diagnostic: "blocked by an earlier gate failure".into(),
        proof: None,
    }
}

fn failed_record(
    context: &GateContext,
    gate: GateId,
    started_at: String,
    class: FailureClass,
    diagnostic: impl Into<String>,
) -> GateRecord {
    GateRecord {
        gate,
        status: GateStatus::Fail,
        started_at,
        ended_at: timestamp(),
        migration_id: context.migration_id.clone(),
        tested_runtime_head_sha: context.tested_runtime_head_sha.clone(),
        source_fingerprint: context.source_fingerprint.clone(),
        destination_fingerprint: context.destination_fingerprint.clone(),
        evidence_refs: vec![format!("failure:{gate}")],
        failure_class: Some(class),
        diagnostic: diagnostic.into(),
        proof: None,
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Driver {
        fail_at: Option<GateId>,
        calls: Arc<Mutex<Vec<GateId>>>,
    }

    #[async_trait]
    impl AcceptanceDriver for Driver {
        async fn readiness(&mut self) -> Result<ReadinessReport, AcceptanceFailure> {
            Ok(ReadinessReport {
                ready: true,
                checks: BTreeMap::from([(String::from("active"), true)]),
                evidence_refs: vec!["readiness:active".into()],
                toolchain: BTreeMap::from([
                    (String::from("opentofu"), String::from("1.12.6")),
                    (
                        String::from("provider"),
                        String::from("terraform-provider-openstack/openstack 3.4.0"),
                    ),
                    (String::from("provider_modified"), String::from("false")),
                ]),
                diagnostic: "active test driver".into(),
            })
        }

        async fn execute_gate(
            &mut self,
            gate: GateId,
            _context: &GateContext,
        ) -> Result<GateProof, AcceptanceFailure> {
            self.calls
                .lock()
                .map_err(|_| AcceptanceFailure::Invariant("test lock poisoned".into()))?
                .push(gate);
            if self.fail_at == Some(gate) {
                return Err(AcceptanceFailure::Driver {
                    class: FailureClass::ExternalService,
                    message: "controlled test failure".into(),
                });
            }
            let mut details = BTreeMap::from([(String::from("observed"), String::from("true"))]);
            match gate {
                GateId::G09 => {
                    details.insert("rollback_migration_id".into(), "migration-a".into());
                }
                GateId::G10 => {
                    details.insert("fresh_migration_id".into(), "migration-b".into());
                }
                GateId::G18 => {
                    details.insert("final_plan".into(), "NO-OP".into());
                }
                GateId::G19 => {
                    details.insert("foreign_state_changes".into(), "0".into());
                    details.insert("project_b_unchanged".into(), "true".into());
                }
                GateId::G20 => {
                    details.insert("owned_leaks".into(), "0".into());
                    details.insert("inconsistencies".into(), "0".into());
                    details.insert("foreign_state_changes".into(), "0".into());
                }
                _ => {}
            }
            Ok(GateProof {
                evidence_refs: vec![format!("proof:{gate}")],
                source_bound: true,
                destination_bound: true,
                details,
            })
        }

        async fn cleanup(
            &mut self,
            _context: &GateContext,
        ) -> Result<GateProof, AcceptanceFailure> {
            Ok(GateProof {
                evidence_refs: vec!["cleanup:inventory".into()],
                source_bound: true,
                destination_bound: true,
                details: BTreeMap::from([
                    (String::from("owned_leaks"), String::from("0")),
                    (String::from("inconsistencies"), String::from("0")),
                    (String::from("foreign_state_changes"), String::from("0")),
                ]),
            })
        }
    }

    fn context() -> GateContext {
        GateContext {
            run_id: "run-a".into(),
            migration_id: "migration-a".into(),
            source_fingerprint: "source-a".into(),
            destination_fingerprint: "destination-a".into(),
            tested_runtime_head_sha: "0123456789012345678901234567890123456789".into(),
        }
    }

    #[tokio::test]
    async fn every_gate_runs_and_cleanup_is_required_for_pass() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let evidence = match AcceptanceHarness::new(
            Driver {
                fail_at: None,
                calls: calls.clone(),
            },
            context(),
        )
        .run()
        .await
        {
            Ok(evidence) => evidence,
            Err(error) => panic!("test driver should pass: {error}"),
        };
        assert_eq!(evidence.execution_result, "passed");
        let call_count = calls.lock().map(|calls| calls.len()).unwrap_or_default();
        assert_eq!(call_count, 20);
        assert!(
            evidence
                .gates
                .iter()
                .all(|gate| gate.status == GateStatus::Pass)
        );
    }

    #[tokio::test]
    async fn failure_stops_later_unsafe_gates_and_marks_them_not_run() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let evidence = match AcceptanceHarness::new(
            Driver {
                fail_at: Some(GateId::G07),
                calls: calls.clone(),
            },
            context(),
        )
        .run()
        .await
        {
            Ok(evidence) => evidence,
            Err(error) => panic!("failure is represented in evidence: {error}"),
        };
        let call_count = calls.lock().map(|calls| calls.len()).unwrap_or_default();
        assert_eq!(call_count, 7);
        assert_eq!(evidence.gates[6].status, GateStatus::Fail);
        assert!(
            evidence.gates[7..]
                .iter()
                .all(|gate| gate.status == GateStatus::NotRun)
        );
        assert_eq!(evidence.execution_result, "failed");
    }

    #[tokio::test]
    async fn every_gate_failure_position_stops_the_journey_at_that_gate() {
        for failed_gate in GateId::ALL {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let evidence = AcceptanceHarness::new(
                Driver {
                    fail_at: Some(failed_gate),
                    calls: calls.clone(),
                },
                context(),
            )
            .run()
            .await
            .expect("failure is represented in evidence");
            let calls = calls.lock().expect("test lock is not poisoned");
            let failed_index = GateId::ALL
                .iter()
                .position(|gate| *gate == failed_gate)
                .expect("failed gate is in the gate list");
            assert_eq!(calls.len(), failed_index + 1);
            assert_eq!(evidence.gates[failed_index].status, GateStatus::Fail);
            assert!(
                evidence.gates[failed_index + 1..]
                    .iter()
                    .all(|gate| gate.status == GateStatus::NotRun)
            );
        }
    }

    #[tokio::test]
    async fn readiness_failure_is_blocked_before_gate_one() {
        struct Blocked;
        #[async_trait]
        impl AcceptanceDriver for Blocked {
            async fn readiness(&mut self) -> Result<ReadinessReport, AcceptanceFailure> {
                Ok(ReadinessReport {
                    ready: false,
                    checks: BTreeMap::new(),
                    evidence_refs: vec!["readiness:missing".into()],
                    toolchain: BTreeMap::new(),
                    diagnostic: "missing environment".into(),
                })
            }
            async fn execute_gate(
                &mut self,
                _gate: GateId,
                _context: &GateContext,
            ) -> Result<GateProof, AcceptanceFailure> {
                Err(AcceptanceFailure::Invariant("must not execute".into()))
            }
            async fn cleanup(
                &mut self,
                _context: &GateContext,
            ) -> Result<GateProof, AcceptanceFailure> {
                Err(AcceptanceFailure::Invariant("must not cleanup".into()))
            }
        }
        let evidence = match AcceptanceHarness::new(Blocked, context()).run().await {
            Ok(evidence) => evidence,
            Err(error) => panic!("blocked readiness is evidence: {error}"),
        };
        assert_eq!(evidence.execution_result, "blocked");
        assert!(
            evidence
                .gates
                .iter()
                .all(|gate| gate.status == GateStatus::Blocked)
        );
    }
}
