# P8 HA current architecture/security review request

Status: **human review recorded**

This request is bound to the current P8 candidate. The independent review is
recorded at
<https://github.com/kubedoio/o3k-rust/issues/651#issuecomment-5328708421>.

## Candidate binding

- Repository: `kubedoio/o3k-rust`
- Source commit: `609bea8fbdbc58fc7db7c00ace828b2dad81aaf8`
- Acceptance artifact:
  `target/p8-ha-acceptance-609bea8/p8-ha-acceptance.json`
- OCI image digest:
  `sha256:8b179e301d4f829e7991740e17bf58ed221847053d11907bd8d2d928a6690c5e`
- Helm chart: `o3k` `0.1.0`
- Packaged Helm digest:
  `sha256:4051742abce793ab890439468ed02c38862cf3188d2e3f34cfedda9593a5de91`
- Percona Operator revision:
  `cf0cd8d4fccb428c8a5f60c86913a33d7257b206`
- Profile: `kubernetes-ha-control-plane`
- Topology: 3 controllers, multiple Kubernetes workers, external HA
  PostgreSQL, shared RWX artifact storage, external KVM/libvirt compute

## Evidence available for review

The strict artifact validates with all twelve canonical scenarios passed. The
campaign also passed the explicit active-create provider-acceptance and
libvirt-success windows, independent cleanup/foreign-state verification, and
the required Rust, Helm, harness-contract, and leak-verifier gates.

The bounded claim is:

> O3K supports the tested Kubernetes HA control-plane profile for beta
> testing, limited to the documented topology and external dependencies.

This does not claim PostgreSQL or storage-cluster HA implementation, regional
DR, zero downtime, or a five-nines SLA.

## Reviewer confirmation requested

The reviewer should independently assess the candidate commit, contracts,
Helm profile, artifact, and campaign evidence, including:

1. controller fencing, leases, stale-owner rejection, and agent mTLS;
2. controller/node loss and automatic agent-owner reconnection;
3. active create/delete failure windows and duplicate-mutation prevention;
4. PostgreSQL failover and one-controller database partition behavior;
5. storage outage fail-closed behavior and running-VM continuity;
6. rolling upgrade and schema-compatible Helm rollback boundaries;
7. two-tenant isolation, cleanup, and unchanged foreign state;
8. non-root controller posture, Secrets, RWX permissions, and version skew;
9. measured recovery bounds and the stated non-claims.

The review must record the exact source commit above and a new HTTPS URL.
The previous approval at
`https://github.com/kubedoio/o3k-rust/issues/651#issuecomment-5325069824`
is bound to commit `6663e2c` and is not valid for this candidate.

Suggested submission:

```text
Reviewer: Senol Colak
Organization: Kubedo GmbH
Role: Architecture and security reviewer
Reviewed commit: 609bea8fbdbc58fc7db7c00ace828b2dad81aaf8
Decision: Ready for Beta tests
Security review: Nothing suspicious found
Scope: P8 Kubernetes HA control-plane profile, limited to the tested topology and documented external dependencies.
```

The required final verdict is:

`DONE — O3K HA control-plane profile proven`
