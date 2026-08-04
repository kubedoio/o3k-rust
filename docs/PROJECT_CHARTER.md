# Project Charter

## Purpose

O3K Rust provides a lightweight OpenStack-compatible control plane for
reproducible test environments first, then evidence-backed edge and small
private-cloud deployments.

O3K implements a declared, executable compatibility profile. It does not treat
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
- integrate with CellHV through the same typed provider model as an optional
  later profile;
- remain operable by small infrastructure teams;
- publish source-bound evidence for compatibility, performance, security,
  cleanup, and recovery claims.

## Primary users

1. infrastructure developers who need ephemeral OpenStack-like environments;
2. storage, network, identity, and SDK teams running integration scenarios;
3. edge operators with small resource budgets;
4. MSPs and SMEs evaluating a supported small private cloud.

## First release outcome

A user can install O3K TestLab on one supported Linux node, use standard
OpenStack CLI commands to authenticate and manage the declared
identity/image/network/placement/compute workflow, execute a real ephemeral-root
QEMU/KVM guest through `o3k-compute` and local `qemu:///system`, inspect the
resource before cleanup, restart the control plane, compute agent, and libvirt,
reconcile without duplication, and remove all O3K-owned state.

Cinder-compatible persistent volumes and `o3k-storage` are a follow-up profile,
not a prerequisite for the first guest. CellHV remains an optional later
provider.

## Development model

O3K uses a contract-first evidence ladder:

```text
ADR/SPEC/contract
-> domain/store/provider tests
-> portable simulated cloud
-> process tests
-> compute/network/storage component gates
-> full-cloud runner
-> failure/restart matrix
-> release gate
```

The protected full-cloud runner verifies integration. It is not the primary
requirements-discovery loop for missing endpoints.

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
- component and full-cloud contract pass rate;
- peak and steady-state memory/CPU footprint;
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
- immediate separate deployment of every logical service or execution agent;
- large-scale multi-region cloud;
- production SLA claims;
- support for every hypervisor, network system, or storage backend;
- direct source compatibility or mechanical translation from another O3K
  implementation.
