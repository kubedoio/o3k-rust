# P13 Implementation Plan — Ecosystem Compatibility & Infrastructure as Code

## Product objective

Existing OpenStack Terraform/OpenTofu configurations that use the resources
listed by the `p13-iac-compatibility-v1` profile and the attribute subsets
frozen by the applicable provider-contract discovery gate should be able to
target O3K through the standard, unmodified `terraform-provider-openstack`
provider while all resulting cloud resources remain canonical O3K resources
and all OpenStack concepts remain compatibility projections. Exact Terraform
attribute subsets are prospective and are frozen by P13.1 for core resources
and by the equivalent P13.3/P13.4 gates for later resource groups.

## Architecture

```text
terraform-provider-openstack  (unmodified upstream v3.4.0)
            |
            v
  OpenStack compatibility API  (Keystone/Glance/Nova/Neutron/Cinder)
            |
            v
  shared O3K application services
            |
            v
        Cloud Kernel
       /     |      \
  compute  network   volume
       \     |      /
        execution providers
```

## Authoritative documents

- **ADR-0175** — OpenStack Ecosystem and Infrastructure-as-Code Compatibility
  Boundary (Accepted 2026-08-24)
- **SPEC-0032** — OpenStack Terraform/OpenTofu Compatibility Profile v1
  (Accepted 2026-08-24)
- **contracts/iac-openstack-profile-v1.yaml** — Machine-readable IaC profile
  contract (Accepted 2026-08-24)

## Phase sequence

### P13.0 — Architecture, compatibility contracts, IaC profile (this phase)

- Product profile definition
- ADR-0175: IaC compatibility boundary
- SPEC-0032: Terraform/OpenTofu compatibility profile
- `contracts/iac-openstack-profile-v1.yaml`: machine-readable contract
- Updated ROADMAP, NORMATIVE_SOURCES, ADR index
- Repository governance tests pass
- **No runtime implementation**

### P13.1 — Real OpenStack provider / OpenTofu behavioral contract discovery

- Real upstream `terraform-provider-openstack` loaded by real OpenTofu
- Real authentication/catalog/discovery through O3K's Keystone-compatible API
- Produce a frozen behavioral contract for the P13.1 core target resources:
  minimal HCL, exact HTTP call traces, field/filter requirements, expected
  status codes, polling behavior, import/read patterns, microversion
  negotiation, error/retry behavior
- No fake provider
- Verify data source reads (`openstack_images_image_v2`,
  `openstack_compute_flavor_v2`)
- The behavioral contract is the evidence baseline for P13.2 implementation.
  P13.3 and P13.4 have mandatory, equivalent discovery gates immediately before
  their implementation; they must freeze their advanced-network and storage
  targets before runtime work in those phases begins.

### P13.2 — Core Image/Compute/Network IaC lifecycle — COMPLETE (bounded profile)

- `openstack_compute_keypair_v2` create/read/delete
- `openstack_networking_network_v2` create/read/update/delete
- `openstack_networking_subnet_v2` create/read/update/delete
- `openstack_networking_port_v2` create/read/update/delete
- `openstack_compute_instance_v2` create/read/update/delete/start/stop/reboot
- Terraform OpenTofu apply/plan/destroy lifecycle for each resource for the
  bounded attribute subset frozen in P13.1
- State file verification
- Provider compatibility at `provider-lifecycle-verified` minimum for the
  declared attribute subset

P13.2 is complete for the declared bounded subsets above. Committed real
provider evidence is recorded in:

- `docs/compatibility/p13-1/p13-2a-provider-lifecycle-evidence.json`
- `docs/compatibility/p13-2/p13-2b-subnet-lifecycle-evidence.json`
- `docs/compatibility/p13-2/p13-2c-provider-port-lifecycle-evidence.json`
- `docs/compatibility/p13-2/p13-2d-provider-server-lifecycle-evidence.json`

This is an operation- and attribute-level claim, not general OpenStack or
general Terraform compatibility. P13.3 remains not implemented; its next gate
is `P13.3A — Security Group / NetworkPolicy provider-contract discovery`.

### P13.3 — Neutron adoption profile — NOT IMPLEMENTED

The P13.3 discovery gate must complete before any P13.3 runtime work begins.

- `openstack_networking_secgroup_v2` — security group CRUD
- `openstack_networking_secgroup_rule_v2` — security group rule CRUD
- `openstack_networking_router_v2` — router CRUD
- `openstack_networking_router_interface_v2` — router interface add/remove
- `openstack_networking_floatingip_v2` — floating IP CRUD
- Neutron API adapter layer over canonical O3K Network/AddressRealm/Policy
- Compatibility projection identity mapping

### P13.4 — Native Volume Cinder projection and Terraform volume lifecycle

- `openstack_blockstorage_volume_v3` — Cinder v3 compatibility over native
  O3K Volume domain
- `openstack_compute_volume_attach_v2` — Nova volume attachment compatibility
- Storage operations through native Volume CRUD with Cinder API projection
- Terraform volume lifecycle verification

### P13.5 — IaC state convergence

- `terraform refresh` — state refresh with accurate API response
- `terraform import` — import existing O3K resources into Terraform state
- Drift detection — create resource through OpenTofu, then mutate/delete the
  same canonical resource through the native O3K API, run `tofu plan` /
  refresh-only, verify the exact drift is detected, re-apply and verify
  convergence. This proves the one-authority model: created via OpenTofu/
  OpenStack projection, mutated via native API, observed again through the
  standard provider.
- Destroy-recreate — verify replacement semantics
- Retry/replay — verify Terraform retry behavior against O3K operation model

### P13.6 — Multi-project security and failure evidence

- Two independent OpenTofu projects targeting separate O3K projects
- Cross-project isolation verification
- Restart/failure matrix: controller restart during IaC operations
- Cross-project resource isolation: each O3K project's resources invisible to the other
- No duplicate provider side effects for replay/retry of the same already-accepted
  canonical O3K Operation (internal execution idempotency)
- Lost-response client case documented: O3K cannot guarantee client-level exactly-once
  creation when Terraform loses the create response and has no resource ID; this case
  is exercised but not called a safety pass

### P13.7 — Full-stack real-host acceptance

- Real-host evidence with complete IaC journey
- Full profile test: auth → data sources → managed resources → state
  convergence → cleanup
- Product profile and compatibility manifest updated
- Evidence documented in `docs/evidence/` or equivalent

## Tool baseline

| Tool | Version | Role |
|---|---|---|
| terraform-provider-openstack | v3.4.0 | Standard upstream, unmodified |
| OpenTofu | v1.12.6 | Mandatory default IaC engine |
| Terraform CLI | latest | Secondary test target |

## Profile identity

- Profile name: `p13-iac-compatibility-v1`
- Parent: `native-rust-testlab`
- Authority: `o3k-implemented`

## Key decisions

See ADR-0175 §1–§13 for the full authority model. Summary:

1. IaC tools are clients, not control-plane authorities.
2. Standard unmodified provider only.
3. No native O3K Terraform provider in P13.
4. Canonical O3K authority unchanged.
5. Network/storage translators are compatibility projections.
6. Security properties are not weakened.
7. OpenTofu is the mandatory default; Terraform is secondary.
8. O3K Operations remain internal; the OpenStack compatibility layer exposes
   standard OpenStack resource lifecycle semantics.
9. Internal O3K execution idempotency is guaranteed for one accepted Operation.
   Client-level exactly-once creation is NOT guaranteed when the upstream
   client loses the response (no durable client token in the OpenStack protocol).

## Files expected to change (across all P13 sub-phases)

- `compatibility/product-profiles.yaml` — add IaC profile
- `docs/compatibility/backlog-inventory.yaml` — add IaC journey
- `docs/compatibility/matrix.yaml` — add IaC-specific entries when implemented
- `docs/adr/ADR-0175-*.md` — new ADR
- `docs/specs/SPEC-0032-*.md` — new SPEC
- `contracts/iac-openstack-profile-v1.yaml` — new contract
- `docs/NORMATIVE_SOURCES.md` — add references
- `docs/ROADMAP.md` — update P12/P13 sections
- `docs/adr/README.md` — add ADR-0175 entry
- `contracts/core-architecture-boundaries.toml` — if new crate added
- `crates/o3k-api/src/lib.rs` — Neutron/Cinder adapter routes (P13.3, P13.4)
- `crates/o3k-network/src/lib.rs` — Neutron projection adapter (P13.3)
- `crates/o3k-volume/src/lib.rs` — Cinder projection adapter (P13.4)
- `tests/` — IaC harness tests (P13.1+)
- `scripts/` — IaC orchestration scripts

## Tests to add

### P13.1
- `tests/p13_1_provider_harness.sh` — OpenTofu + provider bootstrap test

### P13.2
- `tests/p13_2_core_lifecycle.sh` — Core resource lifecycle via OpenTofu

### P13.3
- `tests/p13_3_network_extended.sh` — Security group, router, FIP lifecycle

### P13.4
- `tests/p13_4_storage_lifecycle.sh` — Volume and attachment lifecycle

### P13.5
- `tests/p13_5_state_convergence.sh` — Refresh, import, drift, retry

### P13.6
- `tests/p13_6_multiproject_security.sh` — Multi-project isolation

### P13.7
- Real-host evidence gate with complete IaC journey

## Known uncertainties

1. The exact HTTP response shapes required by
   `terraform-provider-openstack` for each resource may require minor
   adjustments beyond the basic API implementation (e.g., specific field
   names in the response that Terraform's schema expects).
2. Security-group rule semantics may not map one-to-one from Neutron
   rules to O3K NetworkPolicy rules. The mapping must handle idempotency,
   ordering, and rule identity.
3. Router interface add/remove semantics may differ between Neutron's
   API and O3K's AddressRealm gateway model.
4. Terraform's `openstack_compute_instance_v2` resource may expect
   response fields (e.g., `access_ip_v4`, `all_metadata`, `network`
   blocks) that the current Nova-compatible API does not fully populate.
5. Volume attachment via Terraform goes through Nova's
   `os-volume_attachments` API, which is implemented but may need
   additional response fields for Terraform's state tracking.

## Non-goals for P13

- `terraform-provider-o3k` native provider;
- Pulumi;
- Ansible modules;
- tenant web UI;
- metering/billing;
- DNS/Designate;
- LBaaS/Octavia;
- Swift/S3;
- Kubernetes-as-a-service;
- Trove production service;
- arbitrary OpenStack endpoint parity;
- live migration;
- automatic unfenced evacuation;
- new hypervisors;
- XCP-ng;
- Proxmox;
- SR-IOV;
- DPDK;
- multi-region;
- provider/dataplane redesign;
- production SLA claims.
