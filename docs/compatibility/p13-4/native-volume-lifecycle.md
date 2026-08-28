# P13.4 native volume lifecycle evidence

This artifact describes the bounded native Cinder projection. It is generated
from the implementation commit recorded at release time by the P13.4 gate.

Implementation commit exercised by the current provider and lifecycle gates:
`53c1ed1247eed2337375792692f7012e2ee071a8`.

## Authority and profile

`o3k_domain::Volume` persisted by `StorageRepository` is canonical authority.
The Cinder routes under `/v3/{project_id}/volumes` are a compatibility
projection. The existing outbound `o3k-cinder` client is not used by this
native profile.

The verified bounded provider contract is:

* create, list, show, update of name/description/metadata, and delete;
* `size` is GiB at the Cinder boundary and is stored as canonical bytes;
* size changes, source/image/snapshot/backup, retype, multiattach, and online
  resize are outside the profile;
* canonical project ownership is checked before every mutation and read;
* attached volumes are rejected on delete.

Canonical lifecycle is persisted before provider mutation when a provider is
configured. Provider failure leaves durable error/deleting state; provider
state never reconstructs a canonical volume.

## Reproduction record

The repository gate is `tests/p13_4_storage_lifecycle.sh`. The provider gate is
`tests/p13_4_provider_volume_smoke.sh`. The latter uses OpenTofu 1.12.6 and
the unmodified terraform-provider-openstack 3.4.0 binary with SHA-256
`2840ef5e25598f85591cf984825a8a19b9de498782cfe253e6d3e78740fbd5dc`.
The source dependency is Gophercloud v2.8.0. The gate requires an exact
post-apply detailed plan exit code of 0 and a successful destroy.

The test emits only redacted HTTP traces and never persists or prints
credentials. PostgreSQL and real-host evidence are recorded by their
respective gates when those profiles are enabled; this document does not
claim an unexecuted backend or guest result.
