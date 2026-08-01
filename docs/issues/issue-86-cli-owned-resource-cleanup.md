# Issue #86 — CLI-owned resource cleanup evidence

Issue #86 requires a complete real CirrOS OpenStack CLI workflow and a
passing protected-host artifact. That full acceptance remains open and is not
claimed by this repository change.

## Bounded repository implementation

The portable real-libvirt harness now models creation and verified absence of
every dependent OpenStack resource, so its cleanup contract cannot pass from a
fake CLI that merely returns success. Normal CI runs this harness. This is a
fixture and contract improvement only; it does not claim real CirrOS, libvirt,
or protected-runner acceptance.

The public CLI harness now creates a public-only keypair, passes it to the
server, and verifies absence after deleting every resource it created:
server, keypair, flavor, subnet, network, and image. A successful delete
command is insufficient. A delete error is observed before being classified,
because the result may be unknown after an interrupted request. Unproven IDs
remain tracked for best-effort retry by the failure handler and the artifact
reports cleanup failure when absence cannot be established.

The stateful CLI fake includes a regression mode in which dependent-resource
delete commands return success without removing resources. The harness must
reject that run and record failed cleanup.

## Explicit non-goals

- no real OpenStack, libvirt, or CirrOS execution;
- no claim that a guest reaches ACTIVE or receives config-drive data;
- no change to the public API or resource lifecycle semantics;
- no replacement for protected runner leak inventory or host evidence.

Decision: [ADR-0093](../adr/ADR-0093-cli-owned-resource-absence-verification.md).
