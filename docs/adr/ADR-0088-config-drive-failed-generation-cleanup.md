# ADR-0088 — Remove failed config-drive publication temporaries

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, governance

## Context

Config-drive generation writes all guest files into a unique temporary
directory before publishing an instance directory. Errors during file
publication or validation of an existing destination can happen after that
temporary directory exists. Returning directly from those paths left an
unpublished `.instance-*-tmp-*` directory on disk, creating a local artifact
leak and making repeated failures accumulate state.

## Decision

Every failure after temporary-directory creation removes that temporary
directory before returning. Existing published content remains untouched when
ownership validation rejects a replacement. Publication continues to use the
existing atomic rename and backup/restore flow.

This is a repository-side filesystem invariant. It does not create an
ISO/VFAT image, attach media to libvirt, or claim Nova, agent, or guest
cloud-init integration.

## Consequences

Failed config-drive attempts leave no unpublished temporary directory. An
operator can distinguish the managed published instance directory from failed
attempt residue, and a rejected replacement preserves the existing path.

## Public sources

- Rust standard library filesystem rename and removal behavior, accessed
  2026-07-31.
- O3K [atomic config-drive publication](ADR-0024-atomic-config-drive-publication.md).
