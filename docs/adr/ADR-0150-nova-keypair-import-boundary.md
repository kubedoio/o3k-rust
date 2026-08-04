# ADR-0150 — Nova public-keypair import boundary

Status: Accepted for issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, identity, cli, governance

#280.

## Decision

O3K TestLab implements the Nova-compatible public-key import subset at
`/v2.1/{project_id}/os-keypairs`. Keypairs are control-plane resources stored
in a dedicated SQLite table with a uniqueness constraint over authenticated
user, project, and name. Every list, show, delete, and server-create lookup is
scoped by the verified token; URL or client-provided ownership values are not
trusted.

Only public OpenSSH keys are accepted. The service validates the supported
algorithm, RFC4253 key blob, algorithm/blob agreement, and bounded input,
canonicalizes the stored public key, and computes Nova's colon-separated MD5
fingerprint from the decoded blob. Private keys are neither generated nor persisted. A
keypair attached to a server cannot be deleted until the server association
has been removed by successful server deletion.

The provider contract does not receive keypair material. The server association
is durable control-plane state; guest `authorized_keys` injection remains the
separate config-drive lifecycle boundary.

## Alternatives rejected

- Encoding keypairs in the generic resource JSON: this would make uniqueness
  and ownership queries less explicit and race-prone.
- Passing public or private key material to compute providers: the current
  issue requires only control-plane compatibility and guest injection belongs
  to issue #80.
- Generating private keys: it would add one-time secret delivery and retry/
  unknown-outcome semantics not required by the protected workflow.

## Provenance

The public Nova Compute API keypair reference and OpenStack Client keypair
command documentation define the route and import shape:

- https://docs.openstack.org/api-ref/compute/#keypairs-keypairs
- https://docs.openstack.org/python-openstackclient/latest/cli/command-objects/keypair.html

Accessed 2026-08-01. The implementation is an independent Rust design.
