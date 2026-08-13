# ADR-0161 — Keystone trust root and service identity

Status: Superseded
Date: 2026-08-04
Supersedes: none
Superseded-by: ADR-0166
Affected-services: identity, governance

## Supersession

ADR-0166 supersedes the architectural framing in this proposed decision.

The safety requirements developed here remain inputs to ADR-0166 and SPEC-0020:

- one normalized internal authorization context;
- strict durable ID versus display-name separation;
- fail-closed authentication and expiry behavior;
- explicit service identity for service-to-service work;
- preservation of original user/project audit context;
- omission of unsupported services from the OpenStack catalog;
- no bootstrap-admin shortcut for internal service calls;
- no resource-orchestration ownership in the identity subsystem.

The superseding decision changes the authority model:

> Keystone remains an OpenStack-compatible identity and catalog API, while
> O3K IAM becomes the canonical Cloud Kernel identity/authorization model.

This proposed ADR must not be used as authority for making Keystone the
permanent internal identity architecture of O3K.

See:

- [ADR-0165 — O3K Cloud Operating System and Cloud Kernel](ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0166 — O3K IAM and Keystone compatibility](ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md)
- [SPEC-0020 — O3K IAM, Keystone compatibility, catalog, and authorization context](../specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md)
