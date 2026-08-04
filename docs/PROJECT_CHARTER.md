# Project Charter

## Purpose

O3K is a lightweight, Rust-native OpenStack-compatible control plane for three
primary uses:

1. reproducible test environments around selected real OpenStack services;
2. progressively native Rust implementations of declared OpenStack service
   profiles;
3. small edge clouds targeting approximately 10–20 hypervisors.

O3K implements declared, executable compatibility profiles. Endpoint count or
nominal support for an entire OpenStack release is not progress by itself.

The normative product-profile definition is
[SPEC-0024](specs/SPEC-0024-product-profiles-and-claims.md), backed by
[ADR-0163](adr/ADR-0163-product-profiles-and-deployment-posture.md).

## Product promises

- provide a useful minimal profile quickly;
- let a selected real OpenStack service run against O3K without a full DevStack
  control plane when the required satellite APIs are implemented;
- progressively provide O3K-owned Rust implementations of Keystone-, Glance-,
  Nova-, Neutron-, Placement-, and Cinder-compatible profiles;
- expose only documented and executable OpenStack behavior;
- omit unsupported or unverified services from the catalog;
- use Keystone-compatible identity as the common trust and discovery root;
- keep authorization, desired state, scheduling, operations, compensation, and
  reconciliation in the control plane;
- separate control-plane orchestration from bounded compute, network, and
  storage execution;
- recover safely from interruption, duplicate delivery, and unknown outcome;
- use libvirt/KVM as the primary real compute backend through `o3k-compute`;
- define `o3k-network` and `o3k-storage` as stable target execution boundaries
  before activating them as separate processes;
- integrate selected external OpenStack services only through explicit hosted-
  service profiles;
- integrate with CellHV through the same typed provider model as an optional
  later profile;
- remain operable by small infrastructure teams;
- publish source-bound evidence for compatibility, performance, security,
  cleanup, recovery, database, and footprint claims.

## Product profiles

### OpenStack service testbed

A real service such as Cinder runs independently while O3K supplies the
declared identity, catalog, token-validation, image, compute, attachment,
network, or placement surfaces required by the selected test workflow.

The hosted service keeps its own supported database, message bus, workers,
backend, migrations, upgrades, and operational ownership. O3K must not present
an external-hosted service as an O3K implementation.

### Native Rust cloud

O3K owns the declared service APIs, durable state, policy, orchestration,
execution, and evidence. The first real native milestone is an ephemeral-root
QEMU/KVM TestLab. Native Cinder-compatible storage follows later.

### Small edge cloud

O3K grows from one control-plane process into a small multi-host cloud for
approximately 10–20 hypervisors. Multi-host scheduling, fencing, restart,
backup/restore, database, network, storage, failure, and operational claims
require profile-specific evidence.

“Connect to another OpenStack” is not one capability. External Keystone,
endpoint registration, hosted services, external service consumption,
federation, and resource sharing require separate decisions and contracts.

## Primary users

1. OpenStack service developers and CI systems that need a lightweight
   surrounding control plane;
2. infrastructure developers who need ephemeral OpenStack-compatible
   environments;
3. storage, network, identity, SDK, and Terraform teams running integration
   scenarios;
4. edge operators targeting approximately 10–20 hypervisors;
5. MSPs and SMEs evaluating a supported small private cloud.

## First release outcome

A user can install the native TestLab profile on one supported Linux node, use
standard OpenStack CLI commands to authenticate and manage the declared
identity/image/network/placement/compute workflow, execute a real ephemeral-root
QEMU/KVM guest through `o3k-compute` and local `qemu:///system`, inspect the
resource before cleanup, restart the control plane, compute agent, and libvirt,
reconcile without duplication, and remove all O3K-owned state.

The first alpha delivers guest metadata through config-drive/cloud-init. It
does not advertise an HTTP metadata service without a separately accepted and
verified profile.

The external-Cinder service-testbed profile may progress in parallel but does
not replace or block the first native guest release unless a later accepted
release decision changes the gate.

## Database posture

- SQLite is the currently supported default for minimal TestLab and portable
  profiles.
- SQLite support includes explicit concurrency, WAL, crash recovery,
  migrations, backup/restore, and filesystem constraints.
- PostgreSQL is the intended production-oriented and stronger-availability
  profile.
- PostgreSQL is not claimed as supported or recommended for installation until
  a real adapter and conformance evidence exist.

## Footprint posture

The minimal O3K control plane targets an approximately 50 MB steady-state
memory footprint. This is a measured target, not a blanket guarantee.

Every footprint claim identifies the exact profile, O3K processes, build,
host, workload phase, and excluded external dependencies. External Cinder,
RabbitMQ, PostgreSQL, libvirt, QEMU guests, and storage backends are reported
separately.

## Development model

O3K uses a contract-first evidence ladder:

```text
ADR/SPEC/contract
-> domain/store/provider tests
-> portable simulated cloud
-> process tests
-> compute/network/storage component gates
-> full-profile or hosted-service runner
-> failure/restart matrix
-> release gate
```

The protected runner verifies integration. It is not the primary
requirements-discovery loop for missing endpoints.

Normative ownership is listed in
[`docs/NORMATIVE_SOURCES.md`](NORMATIVE_SOURCES.md). Charter and roadmap text are
summaries and do not override the referenced specs and contracts.

## Governance

- Apache-2.0 source code;
- public issues, ADRs, specs, contracts, tests, compatibility manifests, and
  evidence;
- issue-driven changes;
- operation-level and profile-specific OpenStack compatibility claims;
- human approval for architecture, security, licensing, public contracts,
  persistent-state changes, privileged execution, and release decisions;
- LLM agents may research and implement but do not own product decisions or
  human approval.

## Success measures

- time to create a selected service-under-test environment;
- time from clean host to first native server;
- time from a failed workflow to the identified service/execution boundary;
- portable simulated-cloud pass rate;
- hosted-service, component, and full-profile contract pass rate;
- measured per-profile memory, CPU, startup, and lifecycle footprint;
- deterministic reinstall, restart, compensation, backup, restore, and cleanup
  rate;
- percentage of mutations that are idempotent and failure-tested;
- zero foreign-resource modification in acceptance tests;
- successful external service and edge pilot completion.

## Explicit non-goals for the bootstrap and first alpha

- complete OpenStack API parity;
- implementing every Nova, Neutron, Cinder, or Keystone endpoint before
  integration;
- replacing the internal dependencies of an externally hosted OpenStack
  service;
- Cinder or boot-from-volume as a prerequisite for the first ephemeral guest;
- immediate separate deployment of every logical service or execution agent;
- broad federation or unspecified cross-cloud interoperability;
- production SLA, HA, PostgreSQL-support, or fixed-footprint claims without
  executable evidence;
- support for every hypervisor, network system, or storage backend;
- direct source compatibility or mechanical translation from another O3K
  implementation.
