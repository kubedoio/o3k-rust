# TestLab API compatibility baseline

Status: Normative. Release: `v0.2.0-alpha.1`. OpenStack primary target:
`2026.1 Gazpacho`; backward-compatibility profile: `2025.2 Flamingo`.

This document freezes the OpenStack-facing contract for the TestLab vertical
slice. The machine-readable source is
[`testlab-api-baseline.json`](testlab-api-baseline.json). A capability is not
advertised merely because a route exists: it must be classified in that file,
and new public operations require a baseline update and contract evidence.

## Required workflow

The supported path is deliberately finite and ordered:

1. obtain a project-scoped Keystone token;
2. create an image, upload its content, and verify/list it;
3. create a flat network, subnet, and port;
4. create or select a flavor, then list, inspect, and delete it;
5. import an ed25519 public key and verify/list it;
6. create a server, inspect it, then delete it;
7. delete the port, subnet, network, and image.

The exact operation identifiers are the `workflow` array in the JSON baseline.
Placement is an optional supporting API and is not required to complete the
single-host workflow. Cinder, advanced Neutron, HA, live migration, and
multi-node behavior are outside this release.

## HTTP contract

The operation records define success and expected failure statuses. Flavor
create, list, detailed list, show, and delete are required Nova operations in
this alpha subset. The historical protected flavor-list probe returned `405`;
that result remains failed protected-runner evidence and must not be promoted
to a compatibility claim until rerun. The keypair import contract is covered
by the public HTTP harness and includes cleanup.

Project-scoped paths must use the authenticated project. Cross-project paths
are concealed with `404`; missing project context is `400`. Clients omit the
Nova microversion header and target Nova microversion `2.1`. Placement
allocation writes use exactly microversion `1.28`. A requested Nova
microversion above the baseline is rejected with `406`, with one narrow
exception: the external Cinder 28 attachment-delete guard (bug #2004555)
requires `GET` on the server volume-attachment list/show operations at
microversion `2.89`. The 2.89 profile is GET-only on those two routes, emits
the upstream 2.89 field set, and leaves the advertised Nova maximum and the
version discovery document at `2.1`. Only explicitly listed extensions are
supported; unknown extensions return `404`.

Errors are JSON and have an `error` object containing integer `code`, string
`title`, and string `message`. Error responses must not expose credentials,
tokens, private keys, or raw provider payloads. `405` means the method is not
part of this baseline; it is not an invitation to infer support from a route.

## Implementation order and issue mapping

Keystone (#290), Glance (#291), Neutron (#292), and Nova (#293) establish the
service contracts. The vertical integration and operation identity work is
#78, followed by real Glance content (#79), config-drive (#80), real Neutron
ports (#81), Placement (#82), domain/project lifecycle (#83), console output
(#84), reconciliation (#85), complete CLI (#86), failure paths (#87), leak
audit (#88), packaging (#89/#90), benchmark/soak evidence (#91), human review
(#92), and the release gate (#93). The JSON `issue_map` is the machine-readable
mapping; it is intentionally explicit so issue order cannot drift silently.

## Evidence boundary

Official OpenStack API references are normative. The pinned
`python-openstackclient` and `openstacksdk` versions provide client contract
evidence. The public Go O3K repository is a non-normative implementation
reference under [ADR-0151](../adr/ADR-0151-public-go-o3k-reference-policy.md).
Protected end-to-end runs are final verification, not discovery.
