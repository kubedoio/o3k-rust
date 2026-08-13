# ADR-0166 — O3K IAM and the Keystone compatibility boundary

Status: Accepted
Date: 2026-08-12
Human-approval: Senol Colak, 2026-08-12
Supersedes: none
Superseded-by: none
Affected-services: identity, governance, all-first-class-services

## Context

ADR-0161 correctly required one validated authorization context, durable
ID/name separation, service identity, fail-closed token validation, and an
explicit catalog. Those safety properties remain valid.

However, ADR-0161 framed Keystone-compatible identity as the common trust and
service-discovery root of O3K. That is too narrow for the product direction
accepted in ADR-0165.

O3K intends to become a Cloud Operating System where future services can be
added without rebuilding tenant isolation, authorization, service identity,
quota, audit, and resource ownership around OpenStack-specific identity
semantics.

Keystone must therefore remain a first-class compatibility API, but it must not
define the canonical internal authorization architecture.

## Decision

### 1. O3K IAM is canonical

The O3K Cloud Kernel owns the canonical identity and authorization model.

The first stable conceptual types are:

```text
Principal
  UserPrincipal
  ServicePrincipal
  WorkloadPrincipal (future profile)

Scope
  stable ownership/security scope
  compatibility projections such as OpenStack domain/project

Resource
  resource_id
  resource_type
  owner_scope
  region/AZ metadata where applicable
  tags/attributes where supported

Action
  service namespace + typed action name

AuthContext
  authenticated principal
  effective scope
  credential/authentication metadata without secrets
  delegated/original actor where applicable
  service principal where applicable
  request/audit identity
  expiry and assurance metadata

AuthorizationRequest
  principal
  action
  resource
  context

AuthorizationDecision
  allow | deny
  stable reason category
  matched policy identity where safe
```

Exact serialized representations require separate contracts. This ADR fixes the
authority model, not a wire format.

### 2. Authorization is service-neutral

Every first-class service defines a bounded authorization vocabulary.

For example:

```text
compute:CreateServer
compute:DeleteServer
network:CreatePort
volume:AttachVolume
database:CreateInstance   # future example, not a support claim
```

Every action declares:

- resource type or resource collection it targets;
- ownership/scope requirements;
- accepted principal classes;
- service-to-service requirements;
- supported context/condition keys;
- whether the action is public discovery;
- audit requirements.

The authorization engine defaults to deny.

A service must not treat possession of a syntactically valid token as
authorization.

### 3. Resource ownership is a kernel invariant

For O3K-owned resources, the canonical store records a stable owner/security
scope independently from public compatibility path parameters or display
names.

Authorization checks that depend on ownership happen before provider dispatch
or secret-bearing external calls.

A service-specific record that omits required ownership is invalid and must not
silently fall back to a caller-provided project path.

### 4. Keystone is a northbound compatibility adapter

The Keystone-compatible API maps OpenStack concepts into the O3K IAM model:

```text
Keystone user       -> O3K user principal
Keystone project    -> O3K ownership/security scope projection
Keystone domain     -> O3K scope hierarchy/projection where selected
Keystone role       -> O3K compatibility role/policy input
Keystone token      -> credential yielding an O3K AuthContext
Keystone service    -> compatibility service registration
Keystone endpoint   -> compatibility endpoint projection
Keystone catalog    -> OpenStack view of the O3K service registry
```

The mapping must be explicit and testable.

O3K services do not parse Keystone wire tokens or public Keystone response
objects as domain state.

### 5. The OpenStack catalog is a projection of a richer service registry

The O3K service registry is designed to eventually describe more than endpoint
URLs.

A service registration may declare:

- stable service identity and namespace;
- ownership mode;
- versions/API surfaces;
- regions and endpoints;
- resource types;
- action vocabulary;
- capabilities/features;
- health/readiness metadata;
- evidence/claim state where relevant.

The Keystone service catalog remains the OpenStack-compatible projection
containing only the fields and services that the selected compatibility profile
may advertise.

A service that is not implemented or not verified for a selected profile is
not advertised merely because it is registered internally.

### 6. Service-to-service work preserves both actor and service identity

For work performed on behalf of a caller, the effective context preserves:

```text
original actor / principal
+ original ownership scope
+ authenticated calling service principal
+ delegated action
+ audit/request identity
```

A service identity does not automatically inherit administrator authority.

Internal in-process calls may pass typed contexts directly. Cross-process calls
must use an authenticated channel and a bounded delegation/service-identity
contract.

### 7. Policy compatibility and native policy are separate concerns

OpenStack policy names, legacy role conventions, and service-specific policy
files may be supported where compatibility requires them.

They are translated at the compatibility boundary.

The O3K canonical policy model uses stable service/action/resource concepts and
must not require a future O3K-native service to invent an `oslo.policy`-style
configuration merely to become secure.

### 8. The bootstrap profile stays intentionally small

The current alpha may continue to support only the declared password/project
flow required by the OpenStack CLI TestLab.

That does not make password auth, project scope, or the current token format the
permanent O3K IAM architecture.

The bootstrap profile must still preserve:

- stable IDs;
- no secret logging;
- bounded expiry;
- project/scope enforcement;
- restart-safe verification;
- one canonical `AuthContext`;
- fail-closed behavior.

### 9. Richer tenancy is deferred, not assumed

ADR-0165 allows O3K to grow beyond a project-only tenancy model, but this ADR
does not prematurely freeze an AWS-style Organization/Account hierarchy.

The kernel must preserve enough separation between:

- principal identity;
- ownership/security scope;
- display/grouping hierarchy;
- billing/administrative grouping;

that a later tenancy ADR can add organization/account/workspace semantics
without changing every service's authorization model.

### 10. IAM changes are security-critical

Any change to:

- authentication methods;
- credential/token format;
- policy semantics;
- principal classes;
- scope hierarchy;
- service delegation;
- federation;
- privileged/admin access;
- policy condition keys;

requires explicit security review and adversarial tests.

## Consequences

### Positive

- Keystone no longer determines the shape of every future O3K service.
- New services share one authorization language and ownership model.
- Cross-tenant mistakes become kernel/contract violations rather than ad hoc
  service conventions.
- OpenStack compatibility remains available.
- Service-to-service identity becomes explicit and least-privilege capable.
- A richer O3K-native API can use the same IAM without carrying Keystone
  terminology into every contract.

### Negative

- O3K must build and secure a real authorization engine rather than relying on
  role checks scattered through handlers.
- Keystone compatibility requires a translation layer and compatibility tests.
- Policy migration must avoid breaking existing OpenStack workflows.
- Scope and role semantics can become complex if the kernel tries to emulate
  every external IAM system.
- A richer service registry introduces governance/versioning work.

## Rejected alternatives

### Keep Keystone as the canonical O3K domain model

Rejected because it couples every future service to OpenStack tenancy, role,
catalog, and token semantics.

### Let every service define its own authorization model

Rejected because it repeats tenant-isolation and service-identity logic and
creates inconsistent security behavior.

### Centralize authentication but keep authorization in handlers

Rejected because the hardest failures are usually resource/action/ownership
authorization failures, not password validation.

### Copy AWS IAM wholesale

Rejected because O3K has different deployment, compatibility, operator, and
private-cloud requirements. O3K borrows the useful
`Principal × Action × Resource × Context` separation without adopting AWS
wire formats or commercial account assumptions.

### Add organization/account hierarchy immediately

Rejected because the current alpha does not need it and premature tenancy
hierarchy would create migration burden before concrete use cases exist.

## Required follow-up

- SPEC-0020 defines the accepted bootstrap and compatibility mapping in
  executable detail;
- introduce typed action/resource identifiers before the first new native
  service outside the existing OpenStack IaaS domains;
- define service registry/manifest versioning before dynamic service
  registration is exposed;
- add authorization conformance tests reusable by every first-class service;
- keep the current alpha's Keystone-compatible password/project workflow
  release gate unchanged;
- require a separate tenancy ADR before introducing organization/account
  hierarchy as a public product contract.
