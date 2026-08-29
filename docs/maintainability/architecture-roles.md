# Architecture Role Classification

Role mapping for each workspace crate.

| Crate | Role | Description |
|-------|------|-------------|
| `o3k` | unresolved |  |
| `o3k-api` | API / OpenStack compatibility projection | HTTP API routers, OpenStack request/response adapters |
| `o3k-cellhv` | provider adapter / host execution | Privileged host execution adapters (libvirt, LVM, nftables, etc.) |
| `o3k-cinder` | external-service client | External OpenStack service client adapters |
| `o3k-compute` | application/service | Application service implementing use cases above domain ports |
| `o3k-compute-agent` | provider adapter / host execution | Privileged host execution adapters (libvirt, LVM, nftables, etc.) |
| `o3k-compute-bin` | unresolved |  |
| `o3k-config` | diagnostics/upgrade/maintenance | Configuration, diagnostics, operational tooling |
| `o3k-config-drive` | provider adapter / host execution | Privileged host execution adapters (libvirt, LVM, nftables, etc.) |
| `o3k-console` | provider adapter / host execution | Privileged host execution adapters (libvirt, LVM, nftables, etc.) |
| `o3k-controller-protocol` | protocol | Wire protocol definitions |
| `o3k-database-example` | example/evidence | Examples, evidence artifacts |
| `o3k-dhcp` | provider adapter / host execution | Privileged host execution adapters (libvirt, LVM, nftables, etc.) |
| `o3k-domain` | contracts/kernel/domain | Canonical domain types, lifecycle state machines, invariants |
| `o3k-identity` | application/service | Application service implementing use cases above domain ports |
| `o3k-image` | application/service | Application service implementing use cases above domain ports |
| `o3k-kernel` | contracts/kernel/domain | Canonical domain types, lifecycle state machines, invariants |
| `o3k-libvirt` | provider adapter / host execution | Privileged host execution adapters (libvirt, LVM, nftables, etc.) |
| `o3k-native-api` | API / native API | Native O3K resource API |
| `o3k-network` | application/service | Application service implementing use cases above domain ports |
| `o3k-network-bin` | unresolved |  |
| `o3k-network-protocol` | protocol | Wire protocol definitions |
| `o3k-placement` | application/service | Application service implementing use cases above domain ports |
| `o3k-provider` | provider port + adapters | Provider/external-service port definitions + adapters |
| `o3k-provider-contract` | provider contract/protocol | Provider wire protocol contracts (protobuf) |
| `o3k-reconciler` | reconciler/workflow | Reconciliation and compensation orchestration |
| `o3k-scheduler` | application/service | Application service implementing use cases above domain ports |
| `o3k-service-sdk` | protocol / SDK | Shared protocol types, client SDK helpers |
| `o3k-storage` | provider adapter / host execution | Privileged host execution adapters (libvirt, LVM, nftables, etc.) |
| `o3k-store` | persistence port + SQLite adapter | Repository ports + SQLite implementation |
| `o3kd` | unresolved |  |

## Unresolved

- `o3k`
- `o3k-compute-bin`
- `o3k-network-bin`
- `o3kd`
