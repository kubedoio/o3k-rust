# SPEC-0016 — Static Rust and OpenStack compatibility target

Status: Normative

The machine-readable contracts are
[`docs/compatibility/target.json`](../compatibility/target.json) and
[`compatibility/openstack-targets.yaml`](../../compatibility/openstack-targets.yaml).
The associated decision is
[ADR-0153](../adr/ADR-0153-static-rust-and-openstack-release-policy.md).

## Fixed decisions

| Surface | Required value |
| --- | --- |
| Rust compiler/MSRV | exact stable `1.97.1` |
| Primary OpenStack series | `2026.1` / Gazpacho |
| Backward profile | `2025.2` / Flamingo |
| Keystone API | v3 |
| Glance API | v2 |
| Neutron API | v2 |
| Nova API | v2.1; O3K microversion window `2.1`–`2.1` |
| Placement API | v1; O3K allocation window `1.28`–`1.28` |

The profile maxima in the manifest are reference deployment maxima, not O3K
claims. O3K must return correct version-negotiation errors outside its
implemented contiguous window. The release-series/codename pairing
`2026.1`/`Flamingo` is invalid and is rejected by CI. The manifest is parsed
and schema-validated; it is not a substring-only document.

For services with microversions, each version header is an object containing
the header name and a service-qualified `value_template`, for example
`compute {microversion}` or `placement {microversion}`. Header templates must
be used consistently for request and response negotiation.

## Acceptance

`tests/compatibility-target.sh` parses the YAML manifest and checks its closed
schema, exact field types, unique profile/service IDs, valid release pairings,
service-qualified header templates, both release profiles, service mappings,
Nova maxima, implemented windows, and baseline linkage. CI also runs locked
workspace checks and the existing API/compatibility tests.
