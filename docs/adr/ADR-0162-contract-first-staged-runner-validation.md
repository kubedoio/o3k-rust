# ADR-0162 — Contract-first development and staged runner validation

Status: Proposed
Date: 2026-08-04
Supersedes: none
Superseded-by: none
Affected-services: compute, network, governance

## Context

The protected KVM/libvirt runner has been valuable for proving real behavior,
but it has also exposed basic API, scope, orchestration, and evidence-design
gaps one failure at a time. A full-cloud workflow has too many possible failure
boundaries to be an efficient requirements-discovery tool.

At the opposite extreme, delaying all real execution until every planned Nova,
Neutron, Cinder, and Keystone endpoint is implemented would produce a large
untested control plane and defer the most important integration risks.

O3K needs a lifecycle that:

- specifies the supported profile before implementation;
- keeps endpoint count bounded;
- proves cross-service state and compensation without privileged hosts;
- validates compute, network, and storage execution independently;
- uses the full protected runner as a final integration gate;
- preserves enough diagnostic state before cleanup to identify the failing
  boundary.

## Decision

### 1. Freeze a declared compatibility baseline

Before implementing a service slice, O3K records its required operations,
fields, status codes, errors, auth scope, state transitions, dependencies, and
microversion or extension policy.

The baseline is based on official OpenStack specifications and public client
behavior. Public Go O3K may be used only as a non-normative secondary reference
under ADR-0151.

Routes outside the declared profile are unsupported even if partial code
exists.

### 2. Development follows an evidence ladder

Every supported workflow advances through these gates:

1. spec and schema validation;
2. domain, store, and migration tests;
3. provider conformance tests with stateful fakes;
4. portable simulated-cloud integration;
5. process-level contract tests;
6. component real-host validation;
7. full-cloud real-host validation;
8. failure/restart matrix;
9. release evidence.

A later gate does not replace earlier evidence. A route or merged PR without the
required evidence remains unverified.

### 3. The portable simulated cloud is mandatory

The simulated profile uses real:

- Keystone-compatible authentication and authorization;
- HTTP APIs and OpenStack client behavior;
- durable stores and migrations;
- scheduling and Placement allocation;
- desired-state transitions;
- operation journals;
- cross-service compensation;
- reconciliation after restart.

Only provider execution is replaced with deterministic compute, network, and
storage fakes. The fakes support delay, timeout, unknown outcome, stale
observation, partial completion, duplicate delivery, and terminal failure.

### 4. Real-host validation is componentized

Privileged validation is divided into explicit gates:

#### Compute gate

```text
image -> transfer -> base/overlay -> config-drive -> libvirt domain
-> console -> lifecycle -> cleanup
```

#### Network gate

```text
port intent -> TAP -> bridge -> DHCP/binding -> observed connectivity
-> cleanup
```

#### Storage gate

```text
volume intent -> backend create -> attachment preparation -> attach/detach
-> delete -> cleanup
```

#### Full-cloud gate

```text
Keystone -> Glance -> Neutron -> Placement -> Nova
-> optional Cinder workflow -> real guest -> reconciliation -> cleanup
```

Cinder is optional in the first ephemeral-root full-cloud gate and mandatory in
the later persistent-volume gate.

### 5. Diagnostic hold points are supported

A protected workflow may pause before destructive cleanup for a bounded,
manually requested diagnostic window. During the hold point, evidence may
inspect:

- `virsh list --all` and domain XML;
- QEMU process and stderr;
- backing chains and config-drive attachment;
- TAP, bridge, DHCP, and lease state;
- provider command and observation journals;
- Placement allocations;
- owned and foreign resource inventories.

The default automated workflow still cleans up in an `always` path. Diagnostic
mode must be protected, time-bounded, source-bound, and unable to preserve
secrets.

### 6. Runner output is evidence, not a development oracle

The full runner must not be used to discover every missing endpoint. Missing
API requirements are found through the compatibility inventory and portable
contract suite. A runner failure identifies an execution or integration
boundary and must produce a machine-readable first-failure classification.

### 7. Closure states are explicit

Each release-critical issue records independently:

- implementation: missing, partial, or complete;
- portable evidence: missing or passed;
- component real-host evidence: missing or passed;
- full-cloud evidence: missing or passed;
- closure decision and remaining acceptance criteria.

A skipped, ready, mocked, or repository-only result is not real-host evidence.

## Consequences

### Positive

- missing API behavior is found before expensive runner cycles;
- full-cloud failures are easier to localize;
- Cinder and later providers can progress without blocking core compute;
- real infrastructure is tested early enough to expose contract mistakes;
- LLM agents receive bounded, traceable tasks and evidence requirements.

### Negative

- more test profiles and evidence artifacts must be maintained;
- component environments require explicit ownership and cleanup rules;
- compatibility-baseline changes require governance rather than opportunistic
  endpoint additions.

## Rejected alternatives

### Full runner after every PR

Rejected because it is expensive, noisy, privileged, and often diagnoses
requirements too late.

### Implement every service completely before any runner test

Rejected because “complete OpenStack service” is unbounded and delays
hypervisor, network, and storage integration risk.

### One monolithic full-cloud test only

Rejected because a failure does not identify whether API, orchestration,
compute, network, storage, or evidence design is responsible.

### Treat fake providers as sufficient release proof

Rejected because they cannot prove host privilege, libvirt, networking,
storage, or cleanup behavior.

## Required follow-up

- maintain the normative baseline in SPEC-0022;
- maintain workflow and compensation rules in SPEC-0021;
- add component runner workflows before broadening the full-cloud gate;
- add a protected diagnostic hold mode with safe automatic timeout and cleanup;
- update issue templates and status evidence to record the separate closure
  states.
