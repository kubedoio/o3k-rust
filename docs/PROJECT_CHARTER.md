# Project Charter

## Purpose

O3K Rust provides a lightweight OpenStack-compatible control plane for reproducible test environments first, then edge and small private-cloud deployments.

## Product promises

- deploy a useful profile quickly;
- expose a documented subset of OpenStack behavior;
- make unsupported behavior explicit;
- recover safely from interruption and retry;
- separate orchestration from provider execution;
- use libvirt/KVM as the primary real compute backend through a versioned
  provider contract;
- integrate with CellHV through the same contract as an optional later
  provider;
- remain operable by small infrastructure teams;
- publish evidence for compatibility, performance, security, and recovery claims.

## Primary users

1. infrastructure developers who need ephemeral OpenStack-like environments;
2. storage, network, identity, and SDK teams running integration scenarios;
3. edge operators with small resource budgets;
4. MSPs and SMEs evaluating a supported small private cloud.

## First release outcome

A user can install O3K TestLab on one supported Linux node, use standard
OpenStack CLI commands to authenticate and manage a minimal
image/network/server workflow, execute a real QEMU/KVM guest through
`o3k-compute` and local `qemu:///system`, restart the control plane and compute
agent, reconcile, and remove the environment. CellHV remains an optional later
provider.

## Governance

- Apache-2.0 source code;
- public issues, ADRs, specs, contracts, tests, and compatibility evidence;
- issue-driven changes;
- human approval for architecture, security, licensing, and public contracts;
- LLM agents may implement but do not own product decisions.

## Success measures

- time from clean host to first server;
- peak and steady-state memory/CPU footprint;
- contract pass rate for supported workflows;
- deterministic reinstall and recovery rate;
- percentage of mutations that are idempotent and failure-tested;
- external pilot completion;
- mean time to diagnose failed operations.

## Explicit non-goals for the bootstrap phase

- complete OpenStack API parity;
- replacement of all OpenStack services;
- large-scale multi-region cloud;
- production SLA claims;
- support for every hypervisor or storage backend;
- direct source compatibility with another O3K implementation.
