# P13.1B upstream source evidence

The provider source was inspected from the public `v3.4.0` tag of
`terraform-provider-openstack/terraform-provider-openstack`. The relevant
files and functions are:

- `openstack/provider.go`: `configureProvider`, which passes `auth_url`, user,
  project, domain, region, endpoint type, and retry settings into the shared
  Gophercloud auth configuration.
- `openstack/data_source_openstack_images_image_v2.go`:
  `dataSourceImagesImageV2Read`, which calls `images.List(...).AllPages`, sends
  `name`, `sort=name:asc`, and `status=active` for the minimal name case, then
  flattens image fields into state.
- `openstack/data_source_openstack_compute_flavor_v2.go`:
  `dataSourceComputeFlavorV2Read`, which calls `flavors.ListDetail(...).AllPages`
  with the description microversion first and `2.1` fallback, filters by name,
  flattens core flavor fields, and unconditionally calls
  `flavors.ListExtraSpecs(...).Extract`.

For endpoint composition, pinned Gophercloud `openstack/client.go` shows that
`NewImageV2` obtains the selected catalog endpoint and sets
`ResourceBase = endpoint + "v2/"`. The image request package then appends
`images`, so the catalog endpoint must be the unversioned service root for the
provider request to become `/v2/images`.

The pinned Gophercloud source is the public `v2.8.0` tag:

- `openstack/image/v2/images/requests.go` and `urls.go`: `List` appends
  `images` to `ServiceURL`; the list query builder emits the provider fields.
- `openstack/compute/v2/flavors/requests.go` and `urls.go`: `ListDetail` uses
  `flavors/detail`; `ListExtraSpecs` performs GET on
  `flavors/{id}/os-extra_specs` and uses the common GET success handling.
- `openstack/compute/v2/flavors/results.go`: `ListExtraSpecsResult.Extract`
  reads the `extra_specs` object, so an empty collection is represented as
  `{ "extra_specs": {} }`.
- `openstack/image/v2/images/results.go` and
  `openstack/compute/v2/flavors/results.go`: extraction defines the response
  shapes consumed by the provider.

These are provenance records, not copied provider implementation. The exact
release archives and extracted provider checksum remain pinned in
`provider-toolchain.json`.
