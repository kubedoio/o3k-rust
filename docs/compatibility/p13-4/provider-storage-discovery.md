# P13.4A native storage provider discovery

Status: discovery recorded before native runtime changes

## Pinned inputs

- OpenTofu: 1.12.6
- terraform-provider-openstack/openstack: 3.4.0
- provider release source commit: `4fd8eba1f85edfdc7aed2d17bae3f3c814abad41`
- required provider binary SHA-256: `2840ef5e25598f85591cf984825a8a19b9de498782cfe253e6d3e78740fbd5dc`
- provider modified: false
- Gophercloud: `github.com/gophercloud/gophercloud/v2 v2.8.0`

The provider source was inspected from the public tag and the Gophercloud
module was downloaded from its public Go module source. No source was copied
into O3K.

## Minimal configurations

Volume:

```hcl
terraform {
  required_providers {
    openstack = {
      source = "terraform-provider-openstack/openstack"
      version = "= 3.4.0"
    }
  }
}

provider "openstack" {
  auth_url = var.auth_url
  user_name = var.user_name
  password = var.password
  tenant_id = var.project_id
}

resource "openstack_blockstorage_volume_v3" "probe" {
  name = "p13-4-volume-probe"
  size = 1
}
```

Attachment:

```hcl
resource "openstack_compute_volume_attach_v2" "probe" {
  instance_id = var.server_id
  volume_id   = var.volume_id
}
```

## Frozen provider contract

The bounded P13.4 profile uses the provider's required `size` (GiB), optional
`name`, `description`, `metadata`, `volume_type`, and `availability_zone`
fields only where the native canonical model can represent them. Source/image,
snapshot, backup, consistency-group, scheduler-hint, multiattach, online
resize, and advanced retype behavior are outside this bounded profile unless a
later evidence gate proves them.

The volume provider creates with `POST /v3/{project}/volumes` and accepts HTTP
202, polls the volume through `GET /v3/{project}/volumes/{id}`, updates through
`PUT /v3/{project}/volumes/{id}` with HTTP 200, and deletes through
`DELETE /v3/{project}/volumes/{id}`. Listing uses `GET /v3/{project}/volumes`
with provider-generated query parameters when filters are configured. The
sanitized local trace also records the post-delete `GET /v3/{project}/volumes/{id}`
returning 404. Gophercloud's pinned volume timestamp decoder expects
`YYYY-MM-DDTHH:MM:SS.mmm` without a timezone suffix; the native projection
emits that exact wire form.

The attachment provider creates with
`POST /v2.1/{project}/servers/{server_id}/os-volume_attachments` and expects
HTTP 200, reads with the paired server/attachment identifier, and deletes with
the corresponding Nova DELETE operation. The provider state identifier is
`{server_id}/{returned_nova_attachment_id}`; it is not the volume UUID.

## Initial compatibility observation

Before native storage implementation, the current O3K runtime has no native
Cinder volume route or native Nova volume-attachment route backed by the
canonical storage service. The first provider probe therefore records the
bounded incompatibility as an expected discovery failure (Cinder volume
resource cannot complete its initial create/read contract). This is a
discovery result, not implementation evidence.

## Native projection verification

The bounded volume lifecycle was exercised against a fresh local `o3kd`
process with the unmodified provider binary and OpenTofu 1.12.6. The trace
included token creation, volume POST, repeated volume GET polling, DELETE, and
post-delete 404 verification. It was sanitized and stored outside the
repository; no token, password, or provider secret is part of this evidence.

## Authority and deviations

Canonical O3K `Volume` and `VolumeAttachment` records remain authoritative.
Cinder and Nova are projections. The existing outbound `o3k-cinder` client is
the separate external-Cinder profile and is not used as native P13.4 authority.
Provider polling/status values will be mapped from canonical lifecycle state;
provider state, Terraform state, and compatibility rows cannot recreate
canonical desired state.

## Provenance

- Provider source: <https://github.com/terraform-provider-openstack/terraform-provider-openstack/tree/v3.4.0>
- Gophercloud source: <https://github.com/gophercloud/gophercloud/tree/v2.8.0>
- Inspection date: 2026-08-28
