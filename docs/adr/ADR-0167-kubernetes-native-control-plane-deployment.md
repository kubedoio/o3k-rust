# ADR-0167 — Kubernetes-native control-plane deployment

Status: Accepted
Date: 2026-08-13
Human-approval: Senol Colak, 2026-08-13
Supersedes: none
Superseded-by: none
Affected-services: governance, control-plane, persistence, compute, network, storage

## Decision

Kubernetes is a first-class deployment target for the O3K control plane, but it is not a dependency of the O3K Cloud Kernel and does not become the authority for O3K tenant resources.

The production-oriented Kubernetes profile requires PostgreSQL and explicit multi-controller coordination before O3K may claim HA. SQLite remains the minimal single-controller TestLab/portable store.

`o3kd` is designed to become horizontally deployable. Background scheduling, reconciliation, cleanup, and provider dispatch require durable work ownership and fencing; Kubernetes pod replication alone is not sufficient correctness.

Host-local execution remains outside Kubernetes by default. Hypervisors run `o3k-compute` with libvirt/QEMU/KVM, and future network/storage agents remain host-local execution boundaries. Kubernetes operates the O3K control plane; O3K operates the cloud.

Kubernetes-specific configuration, lifecycle, and coordination mechanisms remain adapters. Canonical O3K servers, networks, ports, volumes, operations, and audit state do not become Kubernetes CRDs or pod-local state.

The first Kubernetes packaging target is OCI images plus a small Helm deployment. An O3K Operator is deferred until O3K-specific lifecycle automation justifies it.

The current `v0.2.0-alpha.1` libvirt TestLab release gate is unchanged. Kubernetes, PostgreSQL, and HA remain evidence-gated post-alpha targets.

## Required follow-up

- implement and verify the PostgreSQL store adapter;
- define multi-controller work ownership and fencing;
- define graceful termination and readiness drain behavior;
- ensure pod-local filesystem state is non-authoritative;
- build OCI images and a minimal Helm deployment;
- prove single-controller Kubernetes deployment before HA;
- prove rolling updates, replica failover, and database failover before an HA claim;
- keep host execution external by default;
- keep Kubernetes types out of the Cloud Kernel and domain crates.
