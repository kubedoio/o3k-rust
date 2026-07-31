# ADR-0037 — Make the libvirt package profile select libvirt

## Status

Accepted

## Context

The installer accepted `--profile libvirt` and installed the libvirt compute
service, but the daemon environment omitted `O3K_PROVIDER`. The daemon would
therefore use its safe default, `fake`, on a clean installation.

## Decision

When creating the libvirt daemon environment, write `O3K_PROVIDER=libvirt`
alongside the profile's TLS and compute-agent settings. Existing environment
files are preserved so an operator's explicit configuration is not silently
overwritten.

## Consequences

A clean libvirt installation selects the provider it installed. The packaging
test records the profile-selection contract; clean Ubuntu/Debian installation
evidence remains a host-gated release requirement.
