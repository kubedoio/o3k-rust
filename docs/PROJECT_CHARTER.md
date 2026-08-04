# Project Charter

## Purpose

O3K Rust provides a lightweight OpenStack-compatible control plane for
reproducible test environments first, then evidence-backed edge and small
private-cloud deployments.

O3K implements declared, executable compatibility profiles. It does not treat
endpoint count or nominal support for an entire OpenStack release as progress.

## Product promises

- deploy a useful profile quickly;
- expose a documented subset of OpenStack behavior;
- make unsupported behavior explicit and omit unsupported catalog services;
- use Keystone-compatible identity as the common trust and discovery root;
- keep authorization, desired state, scheduling, operations, compensation, and
  reconciliation in the control plane;
- separate orchestration from bounded compute, network, and storage execution;
- recover safely from interruption, duplicate delivery, and unknown outcome;
- use libvirt/KVM as the primary real compute backend through `o3k-compute`;
- define `o3k-network` and `o3k-storage` as stable target execution boundaries
  before activating them as separate processes;
- support selected external OpenStack services as explicit service-under-test
  profiles without claiming that O3K implements those services;
- integrate with CellHV through the same typed provider model as an optional
  later profile;
- remain operable by small infrastructure teams;
- publish source-bound evidence for compatibility, performance, security,
  cleanup, and recovery claims.

## Primary users

1. infrastructure developers who need ephemeral OpenStack-like environments;
2. storage, network, identity, and SDK teams running integration scenarios;
3. teams testing a real external OpenStack service without deploying the rest
   of a full OpenStack control plane;
4. edge operators with small resource budgets;
5. MSPs and SMEs evaluating a supported small private cloud.

## First release outcome

A user can install O3K TestLab on one supported Linux node, use standard
OpenStack CLI commands to authenticate and manage the declared
identity/image/network/placement/compute workflow, execute a real ephemeral-root
QEMU/KVM guest through `o3k-compute` and local `qemu:///system`, inspect the
resource before cleanup, restart the control plane, compute agent, and libvirt,
reconcile without duplication, and remove all O3K-owned state.

The first alpha delivers guest metadata through config-drive/cloud-init. It
does not advertise an HTTP metadata service without a separately accepted and
verified profile.

Cinder-compatible persistent volumes and `o3k-storage` are a follow-up profile,
not a prerequisite for the first guest. CellHV remains an optional later
provider.

## External service-under-test profiles

O3K may provide the surrounding declared APIs for a real independently running
OpenStack service. The first planned example is external Cinder:

- O3K supplies the selected Keystone-, Glance-, and Nova-compatible satellite
  APIs and catalog behavior;
- the catalog marks Cinder as `external-hosted`, not `o3k-implemented`;
- the real Cinder deployment retains its own supported database, message bus,
  API/scheduler/volume processes, storage backend, upgrades, and migrations;
- O3K does not claim “Cinder without dependencies”; it replaces the rest of the
  control plane needed by the selected test workflow.

This profile is defined in
[SPEC-0023](specs/SPEC-0023-external-cinder-service-under-test.md) and does not
block the first ephemeral-root release.

## Development model

O3K uses a contract-first evidence ladder:

```text
ADR/SPEC/contract
-> domain/store/provider tests
-> portable simulated cloud
-> process tests
-> compute/network/storage component gates
-> full-cloud or hosted-service runner
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
- operation-level OpenStack compatibility claims;
- human approval for architecture, security, licensing, public contracts,
  persistent-state changes, and privileged execution boundaries;
- LLM agents may research and implement but do not own product decisions or
  human approval.

## Success measures

- time from clean host to first server;
- time from failed workflow to identified service/execution boundary;
- portable simulated-cloud pass rate;
- component, full-cloud, and hosted-service contract pass rate;
- peak and steady-state memory/CPU footprint measured per deployment profile;
- deterministic reinstall, restart, compensation, and cleanup rate;
- percentage of mutations that are idempotent and failure-tested;
- zero foreign-resource modification in acceptance tests;
- external pilot completion.

## Explicit non-goals for the bootstrap and first alpha

- complete OpenStack API parity;
- implementing every Nova, Neutron, Cinder, or Keystone endpoint before
  integration;
- replacement of all OpenStack services;
- Cinder or boot-from-volume as a prerequisite for the first ephemeral guest;
- claiming that O3K removes the internal dependencies of an externally hosted
  OpenStack service;
- HTTP metadata-service compatibility in the config-drive-only alpha;
- immediate separate deployment of every logical service or execution agent;
- large-scale multi-region cloud;
- production SLA claims;
- support for every hypervisor, network system, or storage backend;
- direct source compatibility or mechanical translation from another O3K
  implementation.
