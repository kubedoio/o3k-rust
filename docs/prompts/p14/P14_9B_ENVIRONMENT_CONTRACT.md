# P14.9B environment contract

This document is the handoff from issue #877 to #878. It describes inputs to
the executable P14.9 coordinator; it is not real-cloud evidence and does not
promote the P14 profile.

## Process entry point

Use the repository checkout at the exact protected `main` revision and run:

```text
tests/p14_9a_real_two_cloud_acceptance.sh
```

The wrapper invokes `p14-acceptance prerequisites`. The `execute`, `resume`,
`validate`, and `cleanup` modes are explicit process modes of the same binary.
The wrapper and binary must record the tested SHA from the checkout, not a
caller-provided claim.

## Protected inputs

Credentials are supplied only in the protected execution environment:

```text
O3K_P14_SOURCE_AUTH_URL
O3K_P14_SOURCE_USERNAME
O3K_P14_SOURCE_PASSWORD
O3K_P14_SOURCE_USER_DOMAIN       (optional; defaults to Default)
O3K_P14_SOURCE_PROJECT_ID
O3K_P14_SOURCE_PROJECT_B
O3K_P14_SOURCE_CLOUD_ID
O3K_P14_SOURCE_REGION             (optional; defaults to RegionOne)
O3K_P14_SOURCE_ALLOWED_HOSTS
O3K_P14_SOURCE_ALLOW_INSECURE_TLS (only for an explicitly isolated lab)
P14_SOURCE_FLOATING_IP             protected Project A guest address
P14_SOURCE_SSH_PRIVATE_KEY         protected Project A probe key path
P14_SOURCE_VOLUME_SHA256            protected guest-volume checksum
O3K_P14_DESTINATION_URL
O3K_P14_DESTINATION_TOKEN
O3K_P14_DATABASE_URL
O3K_P14_TOFU                       (optional; defaults to tofu)
O3K_P14_PROVIDER_BINARY
O3K_P14_PROVIDER_VERSION           (must be 3.4.0)
O3K_P14_9A_EVIDENCE_OUTPUT         (absolute writable path)
```

The old `O3K_P14_REAL_COMPUTE`, `O3K_P14_REAL_NETWORK`, and
`O3K_P14_REAL_VOLUME` booleans are not readiness evidence and are ignored by
the active probe.

## Active readiness contract

Before G01 the runner must actively prove, and record redacted references for:

* Keystone authentication and bounded discovery in projects A and B;
* Nova, Neutron, Glance, and Cinder visibility through public OpenStack APIs;
* O3K health/readiness plus authenticated Compute, Network, and Volume API
  collection probes;
* PostgreSQL `SELECT 1` and reconnect against the isolated P14 database;
* OpenTofu 1.12.6 process identity and the unmodified OpenStack provider 3.4.0
  binary identity/hash.

The #878 environment must additionally provide the real adapter composition
used by `AcceptanceDriver`. It must reuse `o3k_migration::runner::MigrationRunner`
for migration semantics and provide owned, source-bound evidence for all
twenty gates, including guest probes, packet-path probes, interruption hooks,
restart points, rollback-to-fresh-migration identity, cutover, OpenTofu state
handoff, and final inventory comparison. A readiness boolean or proof file is
not a substitute for those observations.

## Evidence and ownership

Evidence is JSON written only below the protected evidence directory. It must
contain no credentials, tokens, private keys, provider payloads, or sensitive
guest data. Every resource created for the run must carry deterministic run
ownership. Cleanup must report migration-owned leaks, inconsistencies, and
foreign/sentinel changes separately; foreign project B resources are never
eligible for cleanup.

The current environment contract intentionally does not provision a cloud.
That work belongs to #878. Until the real adapter and environment are present,
the executable remains fail-closed and can produce only BLOCKED/FAIL evidence.
