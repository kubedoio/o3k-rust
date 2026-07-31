# ADR-0077 — Fail-closed validation for bounded libvirt create inputs

## Status

Accepted for the bounded libvirt provider create path.

## Context

`LibvirtProvider::create_instance` currently builds a domain with no network
interfaces. Accepting a non-empty `CreateInstanceRequest.network_ids` would
therefore report a domain lifecycle operation while silently dropping the
requested network attachments. The provider also does not yet own the
control-plane work needed to resolve image artifacts, materialize config-drive
images, or prepare and verify TAP interfaces.

## Decision

The provider rejects every create request with a supplied `network_ids` entry,
including blank or otherwise malformed entries, with
`ProviderError::InvalidRequest` before image processing or the libvirt
`define` call. An empty network list retains the existing bounded behavior.
Existing image-source validation in `build_domain_xml` remains authoritative
and continues to run before definition. Placement identifiers remain part of
the durable create intent for scheduler/reconciliation use; they are not host
network realization inputs and are not rejected by this boundary check.

## Consequences

The provider cannot claim success for a request whose network intent it cannot
realize, and invalid network input cannot create a partial domain. Full
network/TAP realization, verified artifact resolution, config-drive
materialization and attachment, and agent-backed create orchestration remain
coupled work under issue #47 and require the corresponding host evidence.
