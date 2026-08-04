# Architecture decision records

This directory is the source location for O3K Rust ADRs. The lifecycle rules
below are proposed by [ADR-0154](ADR-0154-engineering-governance-lifecycle.md)
and are not evidence that the repository has already passed the corresponding
automated audit.

## Status vocabulary

Every ADR must declare exactly one of:

`Draft`, `Proposed`, `Accepted`, `Rejected`, or `Superseded`.

Accepted decisions are immutable in substance. A changed decision gets a new
ADR with a `Supersedes` link; a superseded record remains available and links
to its successor. Architecture, security, licensing, privileged/native,
persistence, public-contract, and release-governance decisions require human
approval before `Accepted`.

## ADR index

| ADR | Subject | Status | Affected Services |
| --- | --- | --- | --- |
| [ADR-0001](ADR-0001-clean-slate-rust.md) | Clean-slate Rust implementation | Accepted | identity, governance |
| [ADR-0002](ADR-0002-testlab-first.md) | TestLab first | Accepted | compute, network, storage, image, placement, governance |
| [ADR-0003](ADR-0003-cellhv-provider-boundary.md) | CellHV through a provider contract | Accepted | compute, network, governance |
| [ADR-0004](ADR-0004-contract-before-breadth.md) | Contract evidence before endpoint breadth | Accepted | network, governance |
| [ADR-0005](ADR-0005-libvirt-primary-compute-backend.md) | Libvirt/KVM as the primary compute backend | Accepted | compute, governance |
| [ADR-0006](ADR-0006-compute-agent-boundary.md) | `o3k-compute` agent boundary and protocol | Accepted | compute, network, governance |
| [ADR-0007](ADR-0007-tonic-rustls-client-auth.md) | Tonic version for compute-agent mutual TLS | Accepted | compute, identity, cli, governance |
| [ADR-0008](ADR-0008-local-libvirt-adapter.md) | Local libvirt adapter boundary | Accepted | compute, governance |
| [ADR-0010](ADR-0010-dhcp-boundary.md) | Isolated DHCP ownership boundary | Accepted | network, governance |
| [ADR-0011](ADR-0011-compute-provider-backends.md) | Selectable compute provider backends | Accepted | compute, governance |
| [ADR-0012](ADR-0012-console-output.md) | Bounded durable console output | Accepted | compute, storage, governance |
| [ADR-0013](ADR-0013-lifecycle-safety-boundaries.md) | Lifecycle safety boundaries for the libvirt provider | Accepted | compute, network, identity, governance |
| [ADR-0014](ADR-0014-nova-create-idempotency.md) | Deterministic Nova create retries | Accepted | compute, governance |
| [ADR-0015](ADR-0015-agent-command-dispatch.md) | Agent command dispatch boundary | Accepted | compute, network, identity, governance |
| [ADR-0016](ADR-0016-durable-lifecycle-operations.md) | Durable lifecycle action and delete operations | Accepted | compute, governance |
| [ADR-0017](ADR-0017-agent-command-router.md) | Authenticated compute-agent command router | Accepted | compute, network, identity, governance |
| [ADR-0018](ADR-0018-scheduler-placement-intent.md) | Persist scheduler and Placement bindings in create intent | Accepted | compute, placement, governance |
| [ADR-0019](ADR-0019-canonical-create-command.md) | Canonical create-command construction | Accepted | identity, governance |
| [ADR-0020](ADR-0020-agent-event-reconciliation.md) | Durable agent-event reconciliation | Accepted | compute, identity, governance |
| [ADR-0021](ADR-0021-agent-event-consumer.md) | Live agent-event consumer boundary | Accepted | compute, identity, governance |
| [ADR-0022](ADR-0022-atomic-image-overlays.md) | Atomic image-overlay publication | Accepted | compute, network, image, governance |
| [ADR-0023](ADR-0023-fake-command-realization.md) | Stateful fake command realization | Accepted | compute, network, image, governance |
| [ADR-0024](ADR-0024-atomic-config-drive-publication.md) | Atomic config-drive publication | Accepted | compute, network, placement, governance |
| [ADR-0025](ADR-0025-atomic-dhcp-publication.md) | Atomic DHCP state publication | Accepted | network, governance |
| [ADR-0026](ADR-0026-tap-reuse-ownership-fencing.md) | TAP reuse ownership fencing | Accepted | network, governance |
| [ADR-0027](ADR-0027-agent-targeted-scheduling.md) | Agent-targeted scheduling contract | Accepted | compute, placement, identity, governance |
| [ADR-0028](ADR-0028-console-offset-publication.md) | Bounded console offset reads | Accepted | compute, governance |
| [ADR-0029](ADR-0029-cli-harness-failure-cleanup.md) | Clean up resources after CLI harness failures | Accepted | compute, network, image, cli, governance |
| [ADR-0030](ADR-0030-dnsmasq-supervision.md) | Own the dnsmasq process lifecycle | Accepted | network, governance |
| [ADR-0031](ADR-0031-resolved-create-command-contract.md) | Typed resolved inputs for agent create commands | Accepted | compute, network, image, governance |
| [ADR-0032](ADR-0032-measurement-authentication-input.md) | Use the configured password in measurement authentication | Accepted | identity, cli, governance |
| [ADR-0033](ADR-0033-cli-list-and-resource-evidence.md) | Record CLI list coverage and resource identities | Accepted | cli, governance |
| [ADR-0034](ADR-0034-placement-atomic-publication.md) | Use unique temporary files for Placement publication | Accepted | placement, governance |
| [ADR-0035](ADR-0035-image-publication-temporaries.md) | Isolate all image publication temporary files | Accepted | image, governance |
| [ADR-0036](ADR-0036-network-metadata-publication.md) | Isolate network metadata publication temporary files | Accepted | network, placement, governance |
| [ADR-0037](ADR-0037-libvirt-profile-provider-selection.md) | Make the libvirt package profile select libvirt | Accepted | compute, governance |
| [ADR-0038](ADR-0038-bounded-console-output-api.md) | Honor bounded console-output requests | Accepted | compute, governance |
| [ADR-0039](ADR-0039-provider-backed-readiness.md) | Report provider-backed daemon readiness | Accepted | compute, network, governance |
| [ADR-0040](ADR-0040-scheduler-agent-eligibility-gate.md) | Gate scheduled placement by authenticated agent eligibility | Accepted | compute, placement, identity, governance |
| [ADR-0041](ADR-0041-agent-console-observation.md) | Emit bounded console observations from compute agents | Accepted | compute, identity, governance |
| [ADR-0042](ADR-0042-libvirt-preflight-status.md) | Do not label libvirt preflight as lifecycle readiness | Accepted | compute, cli, governance |
| [ADR-0043](ADR-0043-console-observation-persistence.md) | Persist sequential agent console observations | Accepted | compute, governance |
| [ADR-0044](ADR-0044-api-agent-console-routing.md) | Route console queries through the fenced agent | Accepted | compute, placement, identity, governance |
| [ADR-0045](ADR-0045-console-observation-correlation.md) | Correlate console observations to the fenced agent | Accepted | identity, governance |
| [ADR-0046](ADR-0046-libvirt-serial-console-device.md) | Include an owned serial console device in domain XML | Accepted | compute, network, governance |
| [ADR-0047](ADR-0047-libvirt-console-stream-read.md) | Provide bounded libvirt console stream reads | Accepted | compute, governance |
| [ADR-0048](ADR-0048-real-libvirt-lifecycle-harness.md) | Make the real-libvirt runner execute the public lifecycle harness | Accepted | compute, cli, governance |
| [ADR-0049](ADR-0049-cli-guest-image-and-waits.md) | Upload the guest image and wait for CLI lifecycle evidence | Accepted | compute, network, image, cli, governance |
| [ADR-0050](ADR-0050-resolved-create-network-invariant.md) | Require a network attachment in resolved create commands | Accepted | compute, network, governance |
| [ADR-0051](ADR-0051-verified-image-artifact-resolution.md) | Verified project-scoped image artifact resolution | Accepted | image, governance |
| [ADR-0052](ADR-0052-libvirt-capability-resource-reporting.md) | Report libvirt compute capacity through the agent contract | Accepted | compute, network, placement, governance |
| [ADR-0053](ADR-0053-control-plane-readiness-on-agent-failure.md) | Reflect compute control-plane startup failure in readiness | Accepted | compute, governance |
| [ADR-0054](ADR-0054-libvirt-discovery-fail-closed.md) | Fail closed on ambiguous libvirt ownership discovery | Accepted | compute, governance |
| [ADR-0055](ADR-0055-libvirt-config-drive-attachment.md) | Read-only libvirt config-drive attachment | Accepted | compute, image, governance |
| [ADR-0056](ADR-0056-placement-provider-synchronization.md) | Durable placement provider synchronization | Accepted | network, placement, governance |
| [ADR-0057](ADR-0057-libvirt-tap-interface-attachment.md) | Existing TAP attachment in libvirt domain XML | Accepted | compute, network, placement, governance |
| [ADR-0058](ADR-0058-owned-tap-deletion.md) | Ownership-checked TAP deletion | Accepted | network, governance |
| [ADR-0059](ADR-0059-libvirt-command-ownership-fencing.md) | Ownership fencing for libvirt agent mutations | Accepted | compute, governance |
| [ADR-0060](ADR-0060-redacted-cli-error-artifacts.md) | Do not persist raw CLI error output in evidence artifacts | Accepted | identity, cli, governance |
| [ADR-0061](ADR-0061-measurement-cleanup-finalization.md) | Finalize measurement cleanup status on process exit | Accepted | cli, governance |
| [ADR-0062](ADR-0062-image-cache-hit-revalidation.md) | Revalidate content-addressed image-cache hits | Accepted | image, governance |
| [ADR-0063](ADR-0063-reset-stops-compute-service.md) | Stop both services before reset cleanup | Accepted | compute, governance |
| [ADR-0064](ADR-0064-required-benchmark-release-gate.md) | Require benchmark evidence for release readiness | Accepted | network, governance |
| [ADR-0065](ADR-0065-dhcp-gateway-binding-conflict.md) | Reject DHCP gateway and fixed-binding conflicts | Accepted | network, governance |
| [ADR-0066](ADR-0066-cellhv-provider-selection.md) | Select the configured CellHV provider | Accepted | compute, network, governance |
| [ADR-0067](ADR-0067-bind-agent-messages-to-authenticated-stream.md) | Bind agent messages to the authenticated stream | Accepted | compute, identity, governance |
| [ADR-0068](ADR-0068-validate-existing-network-bridge.md) | Validate existing bridge and uplink ownership | Accepted | network, governance |
| [ADR-0069](ADR-0069-reconcile-placement-refresh-usage.md) | Reconcile usage during placement inventory refresh | Accepted | compute, network, placement, governance |
| [ADR-0070](ADR-0070-bound-config-drive-network-vendor-data.md) | Bound config-drive network and vendor data | Accepted | network, storage, governance |
| [ADR-0071](ADR-0071-serialize-console-writers.md) | Serialize per-instance console mutations | Accepted | placement, governance |
| [ADR-0072](ADR-0072-libvirt-list-ownership-validation.md) | Validate ownership before listing managed libvirt domains | Accepted | compute, governance |
| [ADR-0073](ADR-0073-scheduler-duplicate-conflict-rollback.md) | Roll back allocations on duplicate-name conflicts | Accepted | compute, placement, governance |
| [ADR-0074](ADR-0074-durable-agent-administrative-state.md) | Durably apply compute-agent administrative state | Accepted | compute, governance |
| [ADR-0075](ADR-0075-harden-cli-lifecycle-evidence.md) | Harden OpenStack CLI lifecycle evidence | Accepted | cli, governance |
| [ADR-0076](ADR-0076-placement-registration-usage.md) | Reconcile usage during provider registration | Accepted | network, placement, governance |
| [ADR-0077](ADR-0077-libvirt-create-fail-closed-inputs.md) | Fail-closed validation for bounded libvirt create inputs | Accepted | compute, network, image, governance |
| [ADR-0078](ADR-0078-release-bundle-installer-binaries.md) | Install prebuilt binaries from release bundles | Accepted | compute, governance |
| [ADR-0079](ADR-0079-release-evidence-freshness.md) | Enforce release-evidence freshness | Accepted | network, governance |
| [ADR-0080](ADR-0080-measurement-process-ownership.md) | Measure only an owned control-plane process | Accepted | network, identity, cli, governance |
| [ADR-0081](ADR-0081-release-evidence-raw-binding.md) | Bind release benchmark summaries to raw evidence | Accepted | governance |
| [ADR-0082](ADR-0082-uninstall-helper-completeness.md) | Remove the complete installed helper set | Accepted | placement, governance |
| [ADR-0083](ADR-0083-custom-prefix-uninstall-safety.md) | Restrict systemd cleanup to the default system layout | Accepted | governance |
| [ADR-0084](ADR-0084-protected-real-host-validation.md) | Protect and honestly report real-host validation | Accepted | compute, network, governance |
| [ADR-0085](ADR-0085-runner-capability-probe.md) | Read-only protected runner capability probe | Accepted | cli, governance |
| [ADR-0086](ADR-0086-libvirt-profile-fail-closed.md) | Reject the unimplemented direct libvirt daemon path | Accepted | compute, governance |
| [ADR-0087](ADR-0087-image-cache-node-safety.md) | Reject non-regular image-cache entries | Accepted | image, governance |
| [ADR-0088](ADR-0088-config-drive-failed-generation-cleanup.md) | Remove failed config-drive publication temporaries | Accepted | compute, governance |
| [ADR-0089](ADR-0089-existing-link-kind-fence.md) | Existing bridge link-kind fence | Accepted | network, governance |
| [ADR-0090](ADR-0090-placement-publication-rollback.md) | Roll back in-memory Placement mutations on publication failure | Accepted | network, placement, governance |
| [ADR-0091](ADR-0091-libvirt-observed-state-projection.md) | Fail closed on ambiguous libvirt lifecycle observations | Accepted | compute, network, governance |
| [ADR-0092](ADR-0092-libvirt-console-ownership-fence.md) | Fence libvirt console reads by domain ownership | Accepted | compute, governance |
| [ADR-0093](ADR-0093-cli-owned-resource-absence-verification.md) | Verify absence of every CLI-owned resource | Accepted | network, image, cli, governance |
| [ADR-0094](ADR-0094-action-unknown-outcome-recovery.md) | Recover lifecycle actions from observed unknown outcomes | Accepted | compute, governance |
| [ADR-0095](ADR-0095-race-safe-resource-leak-evidence.md) | Require stable, redacted resource-leak evidence | Accepted | compute, network, placement, governance |
| [ADR-0096](ADR-0096-clean-install-input-validation.md) | Validate clean-install inputs before mutation | Accepted | compute, governance |
| [ADR-0097](ADR-0097-uninstall-precondition-order.md) | Validate purge ownership before service mutation | Accepted | governance |
| [ADR-0098](ADR-0098-benchmark-raw-freshness.md) | Require fresh raw benchmark evidence | Accepted | governance |
| [ADR-0099](ADR-0099-human-review-evidence-package.md) | Make human review evidence explicit and fail closed | Accepted | identity, governance |
| [ADR-0100](ADR-0100-program-tracker-closure-contract.md) | Keep the program tracker fail closed | Accepted | network, governance |
| [ADR-0101](ADR-0101-release-gate-human-review-binding.md) | Bind release readiness to approved human review | Accepted | network, governance |
| [ADR-0102](ADR-0102-runner-capability-artifact-fencing.md) | Fence protected runner capability artifacts to one workflow attempt | Accepted | placement, governance |
| [ADR-0103](ADR-0103-real-host-artifact-retention.md) | Retain protected real-host evidence for a bounded period | Accepted | governance |
| [ADR-0104](ADR-0104-image-overlay-backing-verification.md) | Verify image overlays before publication | Accepted | compute, image, governance |
| [ADR-0105](ADR-0105-config-drive-manifest-integrity.md) | Verify config-drive manifest integrity before destructive use | Accepted | network, placement, governance |
| [ADR-0106](ADR-0106-network-resource-rollback.md) | Roll back O3K-created host-network resources | Accepted | network, governance |
| [ADR-0107](ADR-0107-placement-release-retry-after-delete.md) | Retry Placement release after provider deletion | Accepted | placement, governance |
| [ADR-0108](ADR-0108-create-conflict-before-placement.md) | Check durable create conflicts before Placement allocation | Accepted | compute, placement, governance |
| [ADR-0109](ADR-0109-nova-shutoff-status-projection.md) | Project powered-off libvirt domains as Nova `SHUTOFF` | Accepted | compute, network, cli, governance |
| [ADR-0110](ADR-0110-nova-delete-console-cleanup.md) | Clean owned console artifacts after successful Nova deletion | Accepted | compute, governance |
| [ADR-0111](ADR-0111-preflight-disk-space-evidence.md) | Fail closed on missing preflight disk-space evidence | Accepted | governance |
| [ADR-0112](ADR-0112-clean-install-path-component-fence.md) | Reject unsafe components in clean-install paths | Accepted | network, governance |
| [ADR-0113](ADR-0113-benchmark-release-eligibility.md) | Require explicit benchmark release eligibility | Accepted | compute, network, governance |
| [ADR-0114](ADR-0114-compute-agent-observation-completeness.md) | Emit observations for every successful agent command | Accepted | compute, governance |
| [ADR-0115](ADR-0115-image-service-cache-bridge.md) | Bridge verified image artifacts into the local cache | Accepted | network, image, governance |
| [ADR-0116](ADR-0116-digest-bound-config-drive-attachment.md) | Bind libvirt config-drive attachment to verified bytes | Accepted | compute, governance |
| [ADR-0117](ADR-0117-deterministic-port-mac-binding.md) | Persist one deterministic MAC binding per network port | Accepted | compute, network, placement, governance |
| [ADR-0118](ADR-0118-placement-name-conflict-before-reservation.md) | Reject deterministic name conflicts before Placement reservation | Accepted | compute, network, placement, governance |
| [ADR-0119](ADR-0119-explicit-agent-resource-state-observations.md) | Propagate explicit resource state in agent observations | Accepted | identity, governance |
| [ADR-0120](ADR-0120-uninstall-prefix-path-fence.md) | Fence uninstall paths before removing files | Accepted | governance |
| [ADR-0121](ADR-0121-benchmark-target-recomputation.md) | Recompute benchmark target results at the release gate | Accepted | compute, governance |
| [ADR-0122](ADR-0122-clean-release-source-provenance.md) | Require a clean source tree before release packaging | Accepted | governance |
| [ADR-0123](ADR-0123-protobuf-baseline-remote-ref.md) | Keep the protobuf compatibility baseline off the checkout branch | Accepted | governance |
| [ADR-0124](ADR-0124-agent-operation-state-fence.md) | Keep agent operation updates separate from resource observations | Accepted | compute, network, identity, governance |
| [ADR-0125](ADR-0125-create-race-placement-release.md) | Release only the losing placement decision in a create race | Accepted | placement, governance |
| [ADR-0126](ADR-0126-console-read-ownership-fence.md) | Revalidate domain ownership before opening a console | Accepted | compute, governance |
| [ADR-0128](ADR-0128-ci-script-validation.md) | Validate scripts used by protected workflows in normal CI | Accepted | network, governance |
| [ADR-0129](ADR-0129-real-libvirt-harness-cleanup-fixture.md) | Make the portable real-libvirt harness model cleanup state | Accepted | compute, network, image, cli, governance |
| [ADR-0130](ADR-0130-release-version-path-fence.md) | Fence release version input before packaging cleanup | Accepted | governance |
| [ADR-0131](ADR-0131-reset-path-fence.md) | Fence reset paths before service or filesystem mutation | Accepted | governance |
| [ADR-0132](ADR-0132-preflight-data-filesystem.md) | Measure the configured data filesystem during preflight | Accepted | compute, governance |
| [ADR-0133](ADR-0133-ci-all-features-test.md) | Exercise every Cargo feature in normal CI | Accepted | compute, cli, governance |
| [ADR-0134](ADR-0134-daemon-placement-wiring.md) | Wire daemon compute requests through Placement | Accepted | compute, placement, identity, governance |
| [ADR-0135](ADR-0135-image-overlay-temporary-recovery.md) | Recover stale image-overlay temporaries on restart | Accepted | compute, image, governance |
| [ADR-0136](ADR-0136-release-output-and-bundle-type-fences.md) | Fence release output roots and bundle file types | Accepted | governance |
| [ADR-0137](ADR-0137-sbom-output-root-fence.md) | Fence the default SBOM output root | Accepted | governance |
| [ADR-0138](ADR-0138-measurement-artifact-ownership.md) | Serialize measurement artifact writers | Accepted | governance |
| [ADR-0139](ADR-0139-installer-owned-file-fence.md) | Preserve foreign installation files | Accepted | placement, governance |
| [ADR-0140](ADR-0140-network-resource-ownership.md) | Fence managed TAPs, DHCP leases, and port MACs | Accepted | network, governance |
| [ADR-0141](ADR-0141-stale-agent-stream-fence.md) | Fence events from replaced agent streams | Accepted | placement, identity, governance |
| [ADR-0142](ADR-0142-image-cache-ownership-fences.md) | Fence image cache directories and temporary artifacts | Accepted | compute, image, governance |
| [ADR-0143](ADR-0143-console-storage-fences.md) | Fence console storage and bounded reads | Accepted | storage, governance |
| [ADR-0144](ADR-0144-observation-watermark-fence.md) | Persist agent observation watermarks | Accepted | compute, governance |
| [ADR-0145](ADR-0145-console-offset-routing.md) | Route nonzero console offsets to durable storage | Accepted | compute, network, storage, governance |
| [ADR-0146](ADR-0146-agent-inventory-publication.md) | Publish authenticated agent inventory to Placement | Accepted | network, placement, identity, governance |
| [ADR-0147](ADR-0147-qcow2-cache-format-verification.md) | Verify qcow2 format before cache publication | Accepted | image, governance |
| [ADR-0148](ADR-0148-durable-command-acceptance.md) | Persist authenticated command acceptance | Accepted | compute, network, identity, governance |
| [ADR-0149](ADR-0149-config-drive-request-boundary.md) | Reject unsupported config-drive server requests | Accepted | compute, network, governance |
| [ADR-0150](ADR-0150-nova-keypair-import-boundary.md) | Nova public-keypair import boundary | Accepted | compute, network, identity, cli, governance |
| [ADR-0151](ADR-0151-public-go-o3k-reference-policy.md) | Public Go O3K as a non-normative reference | Accepted | identity, cli, governance |
| [ADR-0152](ADR-0152-authenticated-artifact-transfer.md) | Bounded authenticated artifact transfer | Accepted | compute, image, identity, cli, governance |
| [ADR-0153](ADR-0153-static-rust-and-openstack-release-policy.md) | Static Rust and OpenStack release policy | Accepted | governance |
| [ADR-0154](ADR-0154-engineering-governance-lifecycle.md) | Engineering governance lifecycle | Proposed | governance |
| [ADR-0155](ADR-0155-agent-local-image-materialization.md) | Agent-local verified image materialization | Proposed | compute, image, identity, governance |
| [ADR-0160](ADR-0160-service-topology-and-execution-boundaries.md) | Service topology and execution boundaries | Proposed | compute, network, storage, image, placement, identity, governance |
| [ADR-0161](ADR-0161-keystone-trust-and-service-identity.md) | Keystone trust root and service identity | Proposed | identity, governance |
| [ADR-0162](ADR-0162-contract-first-staged-runner-validation.md) | Contract-first development and staged runner validation | Proposed | compute, network, governance |
| [ADR-0163](ADR-0163-product-profiles-and-deployment-posture.md) | Product profiles and deployment posture | Proposed | governance |

## Required audit

The ADR audit is accepted only when it can demonstrate, from repository state:

- unique identifiers and parseable required metadata;
- status values limited to the vocabulary above;
- resolvable supersession links and an acyclic graph;
- no duplicate active decision for the same subject without an explicit
  conflict record;
- a fitness function or justified `not-applicable` entry for every accepted
  decision where practical;
- recorded human approval for accepted high-risk decisions.

Malformed metadata, dangling links, cycles, duplicate active decisions, and
unapproved high-risk acceptance must fail closed. A passing link check alone is
not ADR acceptance.
