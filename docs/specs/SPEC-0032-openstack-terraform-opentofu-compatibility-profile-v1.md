# SPEC-0032 — OpenStack Terraform/OpenTofu Compatibility Profile v1

Status: Accepted

Related decision: [ADR-0175](../adr/ADR-0175-openstack-ecosystem-and-iac-compatibility-boundary.md) (human architecture/security approval 2026-08-24; this spec derives acceptance from that decision; reviewed baseline `7b4a352dd719607e72bfb0cad0749c38fe54686e`)
Related contract: [IaC OpenStack profile v1](../../contracts/iac-openstack-profile-v1.yaml)

This specification derives acceptance from ADR-0175. Acceptance authorizes
P13.1 provider-contract discovery; it does not claim that any Terraform
resource lifecycle is implemented or provider-verified. P13.2+ runtime work and
all compatibility claims remain gated by the staged evidence requirements in
this specification.

Primary compatibility reference: OpenStack 2026.1 Gazpacho
Backward compatibility reference: OpenStack 2025.2 Flamingo where declared

Terraform provider reference:
  terraform-provider-openstack/openstack v3.4.0
  https://github.com/terraform-provider-openstack/terraform-provider-openstack
  (Apache-2.0)

OpenTofu reference:
  OpenTofu v1.12.6
  https://github.com/opentofu/opentofu
  (MPL-2.0)

Related normative sources:

- [ADR-0160](../adr/ADR-0160-service-topology-and-execution-boundaries.md)
- [ADR-0163](../adr/ADR-0163-product-profiles-and-deployment-posture.md)
- [ADR-0165](../adr/ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0166](../adr/ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md)
- [ADR-0168](../adr/ADR-0168-o3k-routed-fabric-and-network-execution.md)
- [ADR-0169](../adr/ADR-0169-native-persistent-storage-and-o3k-storage-boundary.md)
- [ADR-0173](../adr/ADR-0173-native-o3k-resource-api-and-resource-model.md)
- [ADR-0174](../adr/ADR-0174-service-manifest-and-resource-provider-controller.md)
- [SPEC-0020](SPEC-0020-keystone-trust-catalog-and-auth-context.md)
- [SPEC-0021](SPEC-0021-cross-service-workflows-and-compensation.md)
- [SPEC-0022](SPEC-0022-service-api-baseline-and-evidence-gates.md)
- [SPEC-0024](SPEC-0024-product-profiles-and-claims.md)
- [SPEC-0025](SPEC-0025-rust-rewrite-and-architecture-convergence.md)
- [SPEC-0026](SPEC-0026-o3k-routed-fabric-v1.md)
- [SPEC-0027](SPEC-0027-native-persistent-storage-v1.md)
- [SPEC-0030](SPEC-0030-native-o3k-resource-api-v1.md)
- [SPEC-0031](SPEC-0031-service-extension-controller-v1.md)
- [execution-boundary contract](../../contracts/execution-boundaries.md)
- [IaC OpenStack profile v1 contract](../../contracts/iac-openstack-profile-v1.yaml)

## Purpose

This specification defines the O3K IaC compatibility profile: which Terraform
resources are targeted, which OpenStack API operations they require, the
evidence tier for each resource, and the specific known deviations from
standard OpenStack behavior.

Resources are listed by name in P13.0. The exact Terraform attribute subset
for each resource (the fields that the provider reads/writes and that O3K must
populate) is declared prospective and will be frozen by the applicable
provider-contract discovery gate: P13.1 for core resources and the mandatory
pre-P13.3/P13.4 gates for later resource groups. Each phase proceeds only
against its own frozen attribute subset.

The profile is operation-level. A resource is not supported simply because
its name appears here; it is supported only when its evidence tier requirements
are met and the corresponding compatibility manifest entry is marked
`implemented`.

## Product profile binding

Profile name: `p13-iac-compatibility-v1`

Parent product identity: O3K Cloud OS

Parent deployment profile: `native-rust-testlab` (extended for IaC)

This profile extends the native-rust-testlab profile. It does not define a
separate cloud authority. It does not alter the first-alpha release gate.

## Authority mode

`o3k-implemented` for all declared resources. Every managed resource is a
canonical O3K resource with an OpenStack compatibility projection.

External-hosted services (external Cinder testbed) remain a separate profile.

## Supported IaC engine

The mandatory supported IaC engine is OpenTofu v1.12.6.

Terraform CLI (latest) may be tested as a secondary target where legally and
operationally appropriate. P13 must never require vendoring, redistributing, or
modifying proprietary/BSL artifacts.

The standard `terraform-provider-openstack/openstack` v3.4.0 provider must
remain unmodified. No O3K fork or patch is permitted.

## Initial P13 resource targets

### Data sources (read-only)

| Terraform resource | OpenStack API | O3K canonical mapping |
|---|---|---|
| `openstack_images_image_v2` | Glance v2 image list/show | `image:image` |
| `openstack_compute_flavor_v2` | Nova v2.1 flavor list/show | `compute:flavor` |

### Managed resources — core compute/network

| Terraform resource | OpenStack API | O3K canonical mapping | P13 phase |
|---|---|---|---|
| `openstack_compute_keypair_v2` | Nova v2.1 keypair CRUD | `compute:keypair` | P13.2 |
| `openstack_networking_network_v2` | Neutron v2 network CRUD | `network:network` | P13.2 |
| `openstack_networking_subnet_v2` | Neutron v2 subnet CRUD | `network:subnet` / AddressRealm | P13.2 |
| `openstack_networking_port_v2` | Neutron v2 port CRUD | `network:endpoint` / port | P13.2 |
| `openstack_compute_instance_v2` | Nova v2.1 server CRUD | `compute:server` | P13.2 |

### Managed resources — advanced network

| Terraform resource | OpenStack API | O3K canonical mapping | P13 phase |
|---|---|---|---|
| `openstack_networking_secgroup_v2` | Neutron v2 security group CRUD | Canonical NetworkPolicy (projection) | P13.3 |
| `openstack_networking_secgroup_rule_v2` | Neutron v2 security group rule CRUD | Canonical NetworkPolicy rule (projection) | P13.3 |
| `openstack_networking_router_v2` | Neutron v2 router CRUD | Canonical AddressRealm gateway/route (projection) | P13.3 |
| `openstack_networking_router_interface_v2` | Neutron v2 router add/remove interface | Canonical route/endpoint (projection) | P13.3 |
| `openstack_networking_floatingip_v2` | Neutron v2 floating IP CRUD | Canonical PublicAddress | P13.3 |

### Managed resources — storage

| Terraform resource | OpenStack API | O3K canonical mapping | P13 phase |
|---|---|---|---|
| `openstack_blockstorage_volume_v3` | Cinder v3 volume CRUD | `volume:volume` | P13.4 |
| `openstack_compute_volume_attach_v2` | Nova v2.1 volume attachment | `volume:volume_attachment` | P13.4 |

## Required OpenStack API operations

For each managed resource, the following OpenStack API operations must be
implemented in the O3K compatibility surface:

### openstack_images_image_v2 (data source)

- `GET /v2/images` (list)
- `GET /v2/images/{id}` (show)

Already implemented. See `docs/compatibility/matrix.yaml` entries for
`image_list` and `image_show`.

### openstack_compute_flavor_v2 (data source)

- `GET /v2.1/{tenant}/flavors` (list)
- `GET /v2.1/{tenant}/flavors/{id}` (show)

Already implemented.

### openstack_compute_keypair_v2

- `POST /v2.1/{tenant}/os-keypairs` (create)
- `GET /v2.1/{tenant}/os-keypairs/{keypair_name}` (show)
- `GET /v2.1/{tenant}/os-keypairs` (list)
- `DELETE /v2.1/{tenant}/os-keypairs/{keypair_name}` (delete)

Already implemented. Known deviation: private-key generation is unsupported;
only public-key import is supported.

### openstack_networking_network_v2

- `POST /v2.0/networks` (create)
- `GET /v2.0/networks/{id}` (show)
- `GET /v2.0/networks` (list)
- `PUT /v2.0/networks/{id}` (update — name, admin_state_up)
- `DELETE /v2.0/networks/{id}` (delete)

Already implemented for create/list/show/delete. Update may need additional
verification.

### openstack_networking_subnet_v2

- `POST /v2.0/subnets` (create)
- `GET /v2.0/subnets/{id}` (show)
- `GET /v2.0/subnets` (list)
- `DELETE /v2.0/subnets/{id}` (delete)
- `PUT /v2.0/subnets/{id}` (update)

Already implemented for create/list/show/delete. Update may need additional
verification.

### openstack_networking_port_v2

- `POST /v2.0/ports` (create)
- `GET /v2.0/ports/{id}` (show)
- `GET /v2.0/ports` (list)
- `PUT /v2.0/ports/{id}` (update)
- `DELETE /v2.0/ports/{id}` (delete)

Already implemented for create/list/show/delete. Update may need additional
verification.

### openstack_compute_instance_v2

- `POST /v2.1/{tenant}/servers` (create)
- `GET /v2.1/{tenant}/servers/{id}` (show)
- `GET /v2.1/{tenant}/servers` (list)
- `PUT /v2.1/{tenant}/servers/{id}` (update — name)
- `DELETE /v2.1/{tenant}/servers/{id}` (delete)
- `POST /v2.1/{tenant}/servers/{id}/action` (start, stop, reboot)

Already implemented for create/list/show/delete/action. Server update and
Terraform-specific response shape may need additional verification.

### openstack_networking_secgroup_v2

- `POST /v2.0/security-groups` (create)
- `GET /v2.0/security-groups/{id}` (show)
- `GET /v2.0/security-groups` (list)
- `PUT /v2.0/security-groups/{id}` (update)
- `DELETE /v2.0/security-groups/{id}` (delete)

These operations require a Neutron security-group compatibility adapter. The
canonical mapping is onto O3K NetworkPolicy. P13.3 implements this.

### openstack_networking_secgroup_rule_v2

- `POST /v2.0/security-group-rules` (create)
- `GET /v2.0/security-group-rules/{id}` (show)
- `GET /v2.0/security-group-rules` (list)
- `DELETE /v2.0/security-group-rules/{id}` (delete)

These operations require a Neutron security-group-rule compatibility adapter
mapped onto canonical NetworkPolicy rules. P13.3 implements this.

### openstack_networking_router_v2

- `POST /v2.0/routers` (create)
- `GET /v2.0/routers/{id}` (show)
- `GET /v2.0/routers` (list)
- `PUT /v2.0/routers/{id}` (update)
- `DELETE /v2.0/routers/{id}` (delete)

These operations require a Neutron router compatibility projection over
canonical AddressRealm gateway/route semantics.

**Identity caveat:** Neutron router has durable lifecycle identity that does
not map one-to-one to any single existing O3K canonical resource. The
compatibility projection's identity persistence, owner scope, uniqueness,
restart reconstruction, mapping cardinality, and deletion semantics must be
frozen by the mandatory P13.3 provider-contract discovery gate before
implementation. The
possibility of a future canonical L3 Gateway/Router resource (not Neutron
specific) must remain open.

P13.3 implements this.

### openstack_networking_router_interface_v2

- `PUT /v2.0/routers/{router_id}/add_router_interface` (add interface)
- `PUT /v2.0/routers/{router_id}/remove_router_interface` (remove interface)

These operations require Neutron router-interface compatibility projection.
P13.3 implements this.

### openstack_networking_floatingip_v2

- `POST /v2.0/floatingips` (create)
- `GET /v2.0/floatingips/{id}` (show)
- `GET /v2.0/floatingips` (list)
- `PUT /v2.0/floatingips/{id}` (update)
- `DELETE /v2.0/floatingips/{id}` (delete)

These operations require a Neutron floating-IP compatibility projection over
canonical PublicAddress semantics. P13.3 implements this.

### openstack_blockstorage_volume_v3

- `POST /v3/{tenant}/volumes` (create)
- `GET /v3/{tenant}/volumes/{id}` (show)
- `GET /v3/{tenant}/volumes` (list)
- `PUT /v3/{tenant}/volumes/{id}` (update)
- `DELETE /v3/{tenant}/volumes/{id}` (delete)

These operations require a Cinder v3 compatibility projection over canonical
O3K Volume domain state. P13.4 implements this for native volumes.

### openstack_compute_volume_attach_v2

- `POST /v2.1/{tenant}/servers/{server_id}/os-volume_attachments` (create)
- `GET /v2.1/{tenant}/servers/{server_id}/os-volume_attachments` (list)
- `GET /v2.1/{tenant}/servers/{server_id}/os-volume_attachments/{id}` (show)
- `DELETE /v2.1/{tenant}/servers/{server_id}/os-volume_attachments/{id}` (delete)

These operations require Nova volume-attachment compatibility projection over
canonical VolumeAttachment semantics. P13.4 implements this.

## Known deviations

All O3K profile deviations from standard OpenStack behavior are documented in
the profile contract (`contracts/iac-openstack-profile-v1.yaml`) and in the
compatibility matrix. The following is a summary:

1. **Image management.** Only local filesystem content backend. Import, multi-
   store, and signature workflows are out of scope. Terraform's
   `image_source_url` is supported through the existing Glance compatibility
   path.

2. **Flavors.** Extra specifications are out of scope. Only id, name, vcpus,
   ram, and disk are supported.

3. **Keypairs.** Private-key generation is unsupported. Only public-key import
   is supported. Terraform's auto-generation of a keypair is not supported;
   users must provide a public key.

4. **Networking.** Routed fabric profile only. Routers, floating IPs, and
   security groups are mapped through compatibility projections. Native Neutron
   router semantics (L3 agent, HA router, DVR) are not implemented.

5. **Security groups.** Mapped onto canonical O3K NetworkPolicy. Full Neutron
   security-group semantics (remote group reference, complex rule composition)
   may have limitations.

6. **Compute instances.** Guest key injection, metadata extensions, config-drive,
   and scheduler extensions have limited support. Terraform's `user_data` and
   `personality` fields are supported through config-drive.

7. **Storage (native).** Volume operations use native O3K Volume domain when
   active. Boot-from-volume and multi-attach are deferred. Online resize is
   not supported in the initial profile.

8. **Operation semantics.** O3K durable Operations remain internal to the Cloud
   Kernel. The OpenStack compatibility layer must expose the exact status
   code(s), body, headers, and polling semantics accepted by the pinned upstream
   provider / Gophercloud call path for each resource operation. The status code
   varies per resource: `compute/v2/servers.Create` accepts `200`/`202` (either
   is valid with a standard Nova server body), `blockstorage/v3/volumes.Create`
   accepts **only** `202`, and most Neutron v2.0 operations accept `200`/`201`.
   The client must never depend on an O3K native Operation API response. Exact
   per-operation status codes belong in the applicable provider-contract
   discovery gate.

## Provider contract discovery phase (P13.1)

Before P13.2 runtime implementation, P13.1 must use real OpenTofu 1.12.6 and
real `terraform-provider-openstack` 3.4.0 to produce a frozen behavioral
contract for the core P13.1 target resources. Equivalent mandatory discovery
gates immediately before P13.3 and P13.4 freeze the advanced-network and
storage targets before their runtime implementation. This staged boundary is
intentional: no phase may implement a target before its provider contract is
frozen. Each contract must capture:

- minimal HCL configuration for each resource;
- exact HTTP call traces (method, path, headers, body, query params) observed
  during provider operations;
- expected request/response field sets;
- accepted status codes and error responses;
- polling behavior (which endpoint, which fields, polling interval, terminal
  states);
- list/query filter behavior;
- import and refresh read patterns;
- microversion negotiation and extension detection;
- error and retry behavior;
- known divergence from standard OpenStack behavior.

Each frozen provider contract is the evidence baseline for its corresponding
implementation phase.

## Evidence tiers

Every resource in the P13 profile has two evidence dimensions:

- **Route implementation**: the OpenStack API endpoint exists, accepts valid
  requests, and returns well-formed responses.
- **Provider compatibility**: the pinned `terraform-provider-openstack` 3.4.0
  converges through plan/apply/read/update/destroy/import for the declared
  attribute subset.

Evidence states for each resource:

- `route-implemented`: the OpenStack API route is implemented in O3K.
- `provider-unverified`: the route exists but has not been tested against the
  pinned provider.
- `provider-partial`: the provider converges for a known subset of attributes;
  some attributes are known to diverge.
- `provider-lifecycle-verified`: the pinned provider converges through the full
  Terraform lifecycle (create/read/update/delete) for the declared attribute
  subset.

### P13.1 — Provider contract discovery
Real upstream provider loaded by real OpenTofu. Authentication, catalog
discovery, and provider configuration are verified. HTTP call traces are
produced for every core P13.1 target resource. The core behavioral contract is
frozen before P13.2 implementation.

### P13.2 — Core lifecycle
OpenTofu can create, read, update, and destroy core compute/network
resources for the bounded attribute subset frozen in P13.1. Evidence includes
Terraform plan/apply output and state file contents showing correct resource
IDs and attributes. Provider compatibility verified at
`provider-lifecycle-verified` minimum for the declared attribute subset.

### P13.3 — Advanced network
Before implementation, run the same real-provider discovery gate for every
P13.3 target and freeze its contract. P13.3 cannot rely on an unverified
P13.1/core trace.
OpenTofu can manage security groups, routers, and floating IPs through the
OpenStack compatibility projection. Evidence includes Terraform lifecycle
output and state verification.

### P13.4 — Storage
Before implementation, run the same real-provider discovery gate for every
P13.4 target and freeze its contract. P13.4 cannot rely on an unverified
P13.1/core trace.
OpenTofu can manage volumes and volume attachments through the OpenStack
compatibility projection. Evidence includes Terraform lifecycle output.

### P13.5 — State convergence
OpenTofu refresh, import, drift detection, destroy-recreate, and
retry/replay semantics are verified.

### P13.6 — Multi-project security
Two independent OpenTofu projects targeting separate O3K projects demonstrate
isolation. Restart/failure matrix proves safety.

### P13.7 — Full-stack acceptance
Real-host evidence with the complete IaC journey. Product profile and
compatibility manifest updated to reflect implemented/verified status.

## Versioning

The profile version is `p13-iac-compatibility-v1`. It is updated when:

- a new Terraform resource is added to the supported set;
- an existing resource's evidence tier advances;
- a new deviation is recorded;
- the provider or OpenTofu baseline version changes.

## Future profile expansion

The following resources are intentionally deferred beyond P13. They may be
proposed in a later milestone:

- `openstack_identity_*` (identity resources managed through O3K IAM directly);
- `openstack_lb_*` (Octavia/LBaaS);
- `openstack_dns_*` (Designate);
- `openstack_objectstorage_*` (Swift/S3);
- `openstack_containerinfra_*` (Magnum/Kubernetes);
- `openstack_sharedfilesystem_*` (Manila);
- `openstack_db_*` (Trove);
- `openstack_vpnaas_*`;
- `openstack_fw_*`.

## Non-goals (within this SPEC)

- A native `terraform-provider-o3k`;
- Pulumi support within P13;
- Ansible modules within P13;
- OpenStack resource types beyond those listed in the profile;
- Production SLA claims;
- Multi-region IaC;
- Cross-cloud Terraform configurations.
