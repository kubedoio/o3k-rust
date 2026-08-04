# ADR-0048 — Make the real-libvirt runner execute the public lifecycle harness

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, cli, governance

## Context

`tests/testlab-libvirt.sh` checked host prerequisites but stopped before
creating or deleting a guest. That made a host with usable libvirt produce no
lifecycle evidence, while the separate OpenStack CLI workflow already covered
the required public operations and cleanup.

## Decision

After validating `virsh`, `qemu-img`, `ip`, `/dev/kvm`, and
`qemu:///system`, the real-libvirt runner invokes
`tests/openstack-cli-libvirt.sh` with the libvirt profile and publishes its
redacted result as `libvirt-result.json`. Missing CLI credentials or endpoint
access remains an explicit `skipped` result; a non-skipped lifecycle failure
preserves the failed artifact and its cleanup status.

The runner does not provision daemons or claim fake-provider results as real
libvirt evidence. It validates the delegated artifact before publishing it.
Operators must first install and configure the libvirt profile, then point the
OpenStack client at that control plane.

## Consequences

The real-libvirt entry point now produces validated lifecycle evidence on a
configured host, while remaining safe and diagnostic on an unconfigured host.
The wrapper does not implement isolated daemon/restart/failure scenarios or
guest boot verification; those remain dependent follow-up work for the full
release gate.
