# ADR-0075 — Harden OpenStack CLI lifecycle evidence

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: cli, governance

## Context

The issue #50 CLI workflow saved `server show` and `server list` responses but
treated successful commands as proof that the created server existed. It also
treated a successful delete command as proof of cleanup. Empty, unrelated, or
no-op stateful responses could therefore produce a false-positive lifecycle
artifact.

## Decision

Validate the JSON returned by the public CLI: `server show` must identify the
created server by ID, and `server list` must contain that ID. After deletion,
accept cleanup only when `server show` returns a recognizable not-found error
or a successful list response proves the ID is absent. Cleanup-on-failure uses
the same absence check, so an API no-op cannot be reported as clean.

The cleanup test uses a stateful OpenStack CLI fake with explicit empty,
unrelated, and no-op-delete modes. The real-libvirt harness contract fake now
models server presence and deletion as well. Raw response and error files
remain local diagnostics and are not copied into the machine-readable result,
which continues to expose only redacted resource IDs and status.

## Consequences

- Empty and unrelated show/list responses fail deterministically.
- Successful delete commands without observable deletion fail the workflow and
  mark cleanup as failed when the resource remains.
- The scripts provide stronger local evidence without claiming guest boot,
  provider reconciliation, or host-level cleanup.
- A trusted real-libvirt run is still required; these stateful fakes do not
  replace real CLI/libvirt acceptance evidence.

## Provenance

This is an independently authored shell/JSON validation decision based on
issue #50's public OpenStack CLI workflow and the repository's redacted
artifact contract. No private implementation, schema, or test was used.
