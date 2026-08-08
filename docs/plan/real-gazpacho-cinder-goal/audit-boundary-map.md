# Phase 1 — Boundary Implementation Map (audit, 2026-08-04)

Repo: `kubedoio/o3k-rust` @ `main` = `1cec5cc` (after PR #453). Audit output for the Real Gazpacho Cinder Service-Testbed goal.

## identity_boundary
- implementation: `crates/o3k-identity/src/lib.rs` — `BootstrapConfig` L446, `seed_identity_defaults()` L464 (service project L491, `cinder` user L522, assignments L558), durable catalog L594 (volumev3 when `cinder_endpoint` L611), `TokenService` L659 `issue` L720 `verify` L791 `verify_details` L851 `auth_context` L869 `catalog` L1046; PBKDF2 `PasswordHash` L288; routes `crates/o3k-api/src/lib.rs` L118-121 (`/v3/auth/tokens`), `issue_token` L486, `validate_token` L551, `check_token` L599, `require_token` L712; migrations 0012/0013; wiring `bins/o3kd/src/main.rs` L343-370 (`O3K_BOOTSTRAP_PASSWORD`, `O3K_TOKEN_SIGNING_KEY`, `O3K_CINDER_PASSWORD`, `O3K_CINDER_ENDPOINT`).
- portable test: identity unit tests L1522-1790 (`service_user_authentication_and_separation`, `expired_token_is_rejected`, `endpoint_records_survive_reload_from_store`, ...); `crates/o3k-api/tests/health.rs` (`keystone_password_scope_returns_signed_subject_token` L168, `keystone_get_and_head_token_validation_and_cinder_catalog` L1079); `tests/portable-service-testbed-gate.sh`.
- real artifact: runner `validated-token.json`; evidence.yaml `cinder_service_user_auth`, `o3k_token_validation_by_cinder`, `catalog_discovery_of_volumev3`.
- failure owner: `o3kd` identity bootstrap + runner "Cinder service user authenticates through O3K" step. GAP: runner scopes service token to project `admin` (L178), not `service`; cinder.conf uses `project_name = service`.

## cinder_client_boundary
- implementation: `crates/o3k-cinder/src/lib.rs` — `CinderClient` L259, `acquire_token` L312, `create_attachment` L355, `show_attachment` L377, `list_attachments` L397, `update_attachment_connector` L422, `complete_attachment` L446, `terminate_attachment` L469, `create_volume` L491, `delete_volume` L550, `wait_until` L576, `ConnectionInfo` L129 (redacted digest L188); fake `testkit.rs`; wiring `bins/o3kd/src/main.rs` L290-306; config `O3K_CINDER_PASSWORD` L361.
- portable test: `crates/o3k-cinder/tests/client.rs` (`attachment_lifecycle_validates_token_through_keystone` L156, `timeout_is_unknown_outcome` L287, ...).
- real artifact: evidence.yaml `real_volume_create`, `real_attachment_lifecycle`; goal `nova-cinder-attachment-result.json`.
- failure owner: `o3k-cinder` client / `AttachmentOrchestrator::map_cinder_error`. GAP: runner calls Cinder via curl, not this client.

## nova_attachment_boundary
- implementation: `crates/o3k-api/src/lib.rs` L168-175 routes (`os-volume_attachments`), handlers `attach_volume` L2553 `list_volume_attachments` L2595 `show_volume_attachment` L2626 `delete_volume_attachment` L2660; orchestrator `crates/o3k-compute/src/attachment.rs` (`AttachmentOrchestrator` L51, phases L32-48, `attach` L77, `detach` L363, `reconcile` L435, compensation L564/L591); store 0011/0014.
- portable test: attachment.rs tests L791-965 (`attach_happy_path_persists_terminal_attached_state`, `repeated_detach_is_idempotent`, `compute_attach_success_with_cinder_completion_failure_compensates`, `unsupported_connection_info_is_rejected_and_compensated`), restart L975-1067; `crates/o3k-api/tests/health.rs` `nova_volume_attachment_lifecycle_list_create_show_delete` L1181.
- real artifact: `nova-cinder-attachment-result.json` per-phase. GAP: runner never exercises Nova attachment API (`compute_attach_via_libvirt: not-executed`); `reconcile()` has no production caller.

## compute_agent_boundary
- implementation: `crates/o3k-compute-agent/src/lib.rs` — `ControlPlaneServer` L2175, `register` L754, `heartbeat` L802, `BlockDeviceCommand` L1406-1415, `CommandJournal` L2334, `AgentClient` L2263, `run_with_executor` L3591 (mTLS client L3638); host `bins/o3k-compute/src/main.rs` — `LibvirtCommandExecutor` L28, `execute` L701 (CollectConnector L1093, AttachDisk L1111, DetachDisk L1162, ObserveDisk L1193), `collect_host_connector` L1309.
- portable test: `crates/o3k-compute-agent/tests/registration_tls.rs`, `crates/o3k-compute-agent/tests/agent_mtls.rs` (emits `O3K_AGENT_MTLS_EVIDENCE=`), unit tests L4179-5505.
- real artifact: `compute-agent-mtls-result.json`, `compute-agent-process-mtls-result.json`, `disposable-testlab-bootstrap.json`. GAP: real Cinder runner never starts o3k-compute.

## libvirt_boundary
- implementation: `crates/o3k-libvirt/src/lib.rs` — `LibvirtAdapter` L616, `attach_disk` L703, `detach_disk` L717, `observe_disk` L725, `read_console` L642, `build_attach_disk_xml` L247 (o3k:disk metadata L270), `build_domain_xml` L165; feature-gated backends L1022-1043; consumed by `bins/o3k-compute/src/main.rs`.
- portable test: libvirt.rs tests L1523-2108 (`attach_disk_xml_binds_durable_volume_identity`, `owned_disk_volume_ids_extracts_only_o3k_disks`, ...); harnesses `tests/real-libvirt-harness.sh`, `tests/testlab-libvirt.sh`.
- real artifact: `libvirt-result.json`, `openstack-cli-result.json`, `console-result.json`. GAP: not exercised by Cinder runner.

## guest_observation_boundary
- implementation: `crates/o3k-api/src/lib.rs` `server_action` L2279 `os-getConsoleOutput` L2305 (bounded, agent dispatch L2351); `bins/o3k-compute/src/main.rs` `ConsoleLog` L1021-1092; `crates/o3k-console/src/lib.rs`; libvirt `read_console`.
- portable test: `crates/o3k-api/tests/health.rs` `registered_agent_console_reads_fall_back_to_durable_cache` L25; libvirt console tests.
- real artifact: `console-result.json`, goal `guest-device-observation.json`. GAP: only serial console exists; no SSH/lsblk mechanism (goal requires bounded non-secret guest observation of the block device).

## cleanup_boundary
- implementation: `scripts/cleanup-disposable-testlab.sh` (run-ID markers, process fences), `cleanup-stale-testlab-processes.sh`, `real-host-pre-run-guard.sh` (baseline inventory), `real-host-post-run-guard.sh` (leak diff), `real-host-owned-inventory.py` (foreign-state digests).
- portable test: `tests/real-host-workflow-guards.sh`, `tests/ci-workflow.sh`.
- real artifact: `real-host-owned-inventory-{baseline,after}.json`, `resource-leak-result.json`, `real-host-workflow-result.json`. GAP: real Cinder runner uses FIXED names (`o3k-vg`, `cinder` DB/MQ user, fixed passwords, fixed state root `/var/lib/o3k-cinder-testbed`); cleanup via `trap cleanup_early` only.

## tempest_boundary
- implementation: `tests/tempest-cinder-subset.sh` (104 lines) — writes only `tests/tempest-evidence/tempest-status.yaml` with `evidence_status: NOT_READY` L43, `version_pinned: false`, `installed: false`; supported/known_unsupported maps L45-95.
- portable test: none for Tempest; manifest governance via `tests/compatibility-target.sh`.
- real artifact: `tempest-status.yaml` (NOT_READY); goal `tempest-cinder-results.xml` + `tempest-cinder-summary.json`. GAP: no real Tempest execution; must pin cinder-tempest-plugin 1.21.0 + compatible Tempest revision.

## Cross-cutting gaps (Phase 2-4 targets)
1. Runner `scripts/real-cinder-testbed-runner.sh` is a direct-curl smoke test: Cinder 24.2.0 (Dalmatian) pin L30, hardcoded connector `compute-1/10.0.0.5/iqn.1993-08.org.debian:01:o3k` L231, never starts o3k-compute, never uses Nova attachment API or typed client.
2. Manifest `compatibility/openstack-targets.yaml` declares gazpacho-2026.1 primary; runner pins Dalmatian — mismatch.
3. Binary name: package `o3k-compute-bin` (Cargo.toml L2); runner references `target/debug/o3k-compute` (wrong; actual `target/debug/o3k-compute-bin`).
4. Attachment `reconcile()` has no production caller in o3kd.
5. Foreign state present on this host: systemd cinder-api/scheduler/volume 24.2.0, MariaDB, RabbitMQ (must be preserved by Phase 4 cleanup).
