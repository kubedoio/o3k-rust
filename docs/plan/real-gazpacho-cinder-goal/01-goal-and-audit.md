# Goal Part 1/3 — Objective, Issues, Audit, and Release Profile

Goal: Execute and Prove the Real Gazpacho Cinder Service-Testbed Profile.
Repository: `kubedoio/o3k-rust` | Starting point: `main` at or after `1cec5ccd7d5f005e571eb5b0219ee84782b19a95`
This is file 1 of 3: `01-goal-and-audit.md` (sections A–D). See `02-protected-runner-and-execution.md` (sections E–M) and `03-evidence-closure.md` (sections N–T).

## A. Objective

Complete the remaining real-service acceptance work for the O3K `openstack-service-testbed` profile.

Prove that a real, pinned OpenStack Cinder deployment can use O3K as its surrounding Identity, catalog, Nova attachment, compute-agent, and libvirt environment without DevStack or a complete OpenStack control plane.

The complete required path:

```text
real Cinder service
→ O3K Keystone-compatible authentication
→ O3K token validation and catalog
→ real Cinder volume
→ O3K-managed Nova server
→ Nova os-volume_attachments API
→ O3K attachment orchestrator
→ typed outbound Cinder client
→ o3kd
→ authenticated o3k-compute agent
→ host connector discovery
→ real iSCSI login
→ libvirt hotplug
→ device visible to the guest
→ Nova detach
→ Cinder attachment termination
→ volume and server deletion
→ complete cleanup
```

Do not mark the goal complete because:

- the runner script exists;
- real Cinder starts;
- a volume reaches `available`;
- Cinder's attachment API accepts a hardcoded connector;
- portable tests pass;
- the mock Cinder component test passes;
- a Tempest status file says `NOT_READY`.

The defining evidence is a real volume attached to a real O3K-managed libvirt guest through the public Nova attachment path.

Treat the following accepted implementation as foundations unless `main` proves a regression: durable Keystone identity, typed Cinder client, durable attachment phases, compensation/unknown-outcome handling, compute-agent block-device commands, libvirt iSCSI hotplug, Nova `os-volume_attachments` routes, component mock test, portable gates, compatibility manifests. Do not rewrite these before a real protected run identifies a concrete defect.

## B. Authoritative issues

- #420 — real-service Keystone acceptance
- #421 — real Nova-to-Cinder-to-compute attachment acceptance
- #424 — real Tempest and compatibility evidence
- #429 — protected real Cinder service-testbed profile (primary execution issue)
- #432 — tracker

Keep all open until their original acceptance criteria are proven. Do not create duplicate replacement issues. Create a focused defect issue only when a protected run proves a specific implementation defect that cannot be coherently fixed under #429 or #421.

## C. Phase 1 — Audit the current real runner before execution

Inspect:

```text
scripts/real-cinder-testbed-runner.sh
scripts/component-cinder-mock.sh
tests/tempest-cinder-subset.sh
.github/workflows/
crates/o3k-cinder/
crates/o3k-compute/src/attachment.rs
crates/o3k-compute-agent/
crates/o3k-libvirt/
bins/o3kd/
bins/o3k-compute/
compatibility/openstack-targets.yaml
SPEC-0020 through SPEC-0024
ADR-0160 through ADR-0163
```

Produce a short implementation map:

```yaml
identity_boundary:
cinder_client_boundary:
nova_attachment_boundary:
compute_agent_boundary:
libvirt_boundary:
guest_observation_boundary:
cleanup_boundary:
tempest_boundary:
```

Confirm every boundary has an implementation path, a portable test, a real-run evidence field, and an explicit failure owner.

## D. Phase 2 — Correct the Cinder release profile

O3K's primary OpenStack target is 2026.1 Gazpacho. Pin:

```text
OpenStack series: 2026.1 Gazpacho
Cinder: 28.0.0
cinder-tempest-plugin: 1.21.0
```

Do not silently use Cinder 24.2.0 as Gazpacho evidence. When Ubuntu packages do not supply Gazpacho, choose one reproducible method:

- a pinned official source checkout and Python virtual environment;
- pinned OCI/Kolla images for only the required Cinder services;
- another documented, reproducible isolated installation.

Do not deploy DevStack or an entire OpenStack control plane. The existing 24.2.0 script may remain only as a separately named secondary profile, e.g. `legacy-dalmatian-cinder-testbed`. Its evidence must never satisfy the Gazpacho profile.

Record: exact Cinder version; exact Git commit or image digest; exact client versions; exact Cinder Tempest plugin commit/version; installation method; host distribution; kernel; libvirt; QEMU; LVM and iSCSI implementations.
