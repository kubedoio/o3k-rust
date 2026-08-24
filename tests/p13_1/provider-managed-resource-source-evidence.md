# P13.1C pinned provider source evidence

This is discovery evidence for `terraform-provider-openstack/openstack` 3.4.0
with Gophercloud v2.8.0. The source was inspected from the pinned upstream
checkout used for this gate; it is not copied into O3K and is not a normative
implementation source.

| Resource | Provider 3.4.0 source proof | Gophercloud operation | Minimum HCL | Import |
| --- | --- | --- | --- | --- |
| `openstack_compute_keypair_v2` | `resource_openstack_compute_keypair_v2.go:12-167`; create, read, delete; no Update | `keypairs.Create/Get/Delete`; Nova `os-keypairs` | `name`, `public_key` | keypair name |
| `openstack_networking_network_v2` | `resource_openstack_networking_network_v2.go:24-476`; create, read, update, delete | `networks.Create/Get/Update/Delete`; Neutron `/v2.0/networks` | `name` | network UUID |
| `openstack_networking_subnet_v2` | `resource_openstack_networking_subnet_v2.go:18-507`; create, read, update, delete | `subnets.Create/Get/Update/Delete`; Neutron `/v2.0/subnets` | `network_id`, `cidr`, `ip_version = 4` | subnet UUID |
| `openstack_networking_port_v2` | `resource_openstack_networking_port_v2.go:23-715`; create, read, update, delete | `ports.Create/Get/Update/Delete`; Neutron `/v2.0/ports` | `network_id`, `name` | port UUID |
| `openstack_compute_instance_v2` | `resource_openstack_compute_instance_v2.go:34-1289`; create, read, update, delete, importer | `servers.Create/Get/Delete`, `Start`, `Stop`, `Reboot`; Nova `/servers` | `name`, `image_id`, `flavor_id`, one `network { uuid = ... }` | server UUID |

## Source-proven semantics

- Keypairs require `public_key` for the bounded profile. `private_key` and
  provider key generation are optional/computed paths and are explicitly
  deferred. Read uses `keypairs.Get` by the stored keypair name; delete accepts
  the Gophercloud delete result and has no update path.
- Networks send the provider-expanded `name` plus optional values present in
  the schema. `admin_state_up` and `shared` are in-place updates; `tenant_id`,
  `external`, and provider/network type fields are replacement or computed
  behavior. The provider reads by UUID after create/update.
- Subnets require `network_id`, `cidr`, and `ip_version`; omitted gateway/DHCP
  and allocation-pool fields are provider defaults. Read/update are by UUID.
  The provider can express multiple subnets; O3K cardinality must therefore be
  resolved by the implementation contract, not assumed from the schema.
- Ports require `network_id`. `fixed_ip` conflicts with `no_fixed_ip`; the
  nested fixed-IP object requires `subnet_id` and may carry an `ip_address`.
  `admin_state_up`, `name`, and bounded device fields are update candidates;
  network identity and fixed-IP shape are replacement behavior. Read is by UUID.
- Servers create through `servers.Create`, then poll `servers.Get` until the
  configured `status` (default `ACTIVE`) or a failure state. The minimal
  request uses name, image, flavor, and network UUID. Read enriches state from
  the server response and address map. Update includes the provider's in-place
  name/power/metadata paths; image, flavor, network, and other ForceNew fields
  are replacement. Stop/start/reboot use Nova action POSTs and subsequent
  polling. Delete stops an active server where required, deletes it, and
  treats the terminal not-found result as deletion.

Exact source locations are retained as provenance; the black-box artifact is
the authority for which of these calls O3K actually exposed and which fields
were observed.

The corresponding pinned Gophercloud request/result code is in the v2.8.0
checkout at `openstack/compute/v2/keypairs/requests.go`,
`openstack/networking/v2/networks/requests.go`,
`openstack/networking/v2/subnets/requests.go`,
`openstack/networking/v2/ports/requests.go`, and
`openstack/compute/v2/servers/requests.go` plus `results.go`. These are the
request builders and `Extract` contracts used by the provider source above;
the P13.1C trace freezes the paths/statuses actually reached against O3K.
