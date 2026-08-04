# ADR-0152 — Bounded authenticated artifact transfer

Status: Accepted additive contract decision; runtime implementation is a follow-up.
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, image, identity, cli, governance

## Context

`CreateCommand` intentionally contains opaque artifact IDs and digests, not
host paths or source-store credentials. A future compute agent must therefore
receive verified image and config-drive bytes before it can assemble a local
libvirt domain. Sending paths, XML, shell text, or credentials would violate
the compute-agent boundary and make ownership and restart recovery ambiguous.

The transfer must remain bounded by the existing protobuf message limit, be
safe across mTLS reconnects, and never turn a lost acknowledgement into a
second copy or an assumed VM result.

## Decision

Add command-correlated `ArtifactOffer`, `ArtifactChunk`, `ArtifactEnd`,
`ArtifactAck`, and `ArtifactStatus` messages to the existing bidirectional
`Control` stream. The only initial kinds are `IMAGE_BASE` and
`CONFIG_DRIVE_ISO`. The offer binds transfer, command, operation, resource,
agent, artifact, kind, digest, size, format, chunking, and expiry identities.
Chunks bind index, offset, bytes, and a per-chunk SHA-256; `ArtifactEnd` binds
the final digest and exact size.

The contract limits every artifact, including the final config-drive ISO, to
64 MiB (67,108,864 bytes) and every chunk payload to 256 KiB (262,144 bytes),
for at most 256 chunks. Transfer implementations must also bound concurrency
before allocation; the initial target is two concurrent transfers per agent
and four in-flight chunks per transfer. Only contiguous sequential writes are
accepted, with per-chunk and whole-artifact verification before commit.

The control plane persists delivery intent before sending an offer and sends a
create command only after all required artifacts are durably `COMMITTED`. An
equivalent duplicate offer or chunk is idempotent; a conflicting duplicate is
rejected. Lost acknowledgements are unknown transport outcomes and reuse the
same transfer identity.

The agent persists a transfer manifest and partial data under its own private
artifact root. The manifest binds stable agent identity, transfer and command
correlation, artifact metadata, digest, size, format, chunk count, committed
offset, and the O3K ownership marker. On restart, the agent obtains a new
`agent_epoch`, retains only valid owned partial/committed state, reports status,
and resumes only the same transfer identity. Missing, foreign, symlinked,
tampered, or conflicting state fails closed; foreign data is never deleted.
The control plane reuses durable transfer IDs after restart and resumes from
the agent's acknowledged contiguous offset. Epoch fencing protects the stream
and stale acknowledgements, while stable agent identity preserves artifact
ownership across reconnects.

## Wire compatibility decision

Keep `o3k.compute.v1` at wire revision 1. The change adds new oneof alternatives
and new enum values without reusing fields, removing fields, or changing the
meaning of existing envelopes. Existing peers can ignore unknown alternatives;
the control plane must negotiate an artifact-transfer capability before sending
these messages and must not send a create command whose artifacts the peer
cannot receive. A future change that alters command identity, transfer-state
meaning, or the existing operation state machine requires a new wire revision
or package.

The repository has no checked-in protobuf descriptor baseline. CI runs `buf
breaking` against the remote `origin/main` schema as specified by ADR-0123;
the additive change is therefore validated against the current default branch
without changing a baseline file.

## Alternatives rejected

- Put image/config-drive bytes inside `CreateCommand`: exceeds message bounds,
  couples command identity to transfer retries, and prevents resumable delivery.
- Send host-local paths or libvirt XML: leaks privileged topology and bypasses
  agent ownership checks.
- Use a separate unauthenticated file endpoint: duplicates authorization and
  stream-fencing logic and permits cross-command confusion.
- Advance the wire revision for additive envelopes: would unnecessarily reject
  compatible legacy peers and conflicts with the repository's additive-v1
  policy.

## Consequences and non-goals

This decision defines the wire contract and safety invariants only. It does not
implement transfer persistence, image/config-drive readers, host networking,
libvirt domain assembly, scheduling, guest boot, or Nova API behavior. The
implementation must add contract, bounds, digest, restart, ownership,
epoch-fencing, and mTLS tests before claiming this ADR is realized.

## Public references and provenance

- gRPC authentication guidance: <https://grpc.io/docs/guides/auth/> (accessed
  2026-08-02);
- Protocol Buffers language guide, field compatibility and unknown fields:
  <https://protobuf.dev/programming-guides/proto3/> (accessed 2026-08-02);
- RFC 8446, TLS 1.3: <https://www.rfc-editor.org/rfc/rfc8446> (accessed
  2026-08-02);
- O3K `ADR-0006`, `ADR-0031`, `ADR-0123`, and `SPEC-0015`;
- no Go source, private source, schema, test, or fixture was copied or
  adapted.
