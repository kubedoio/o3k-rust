# Claude and coding-agent orientation

The canonical agent rules are in [`AGENTS.md`](AGENTS.md). This file adds a compact startup checklist for coding sessions.

## Start every session

1. Identify one GitHub issue.
2. Read `AGENTS.md` and linked specs.
3. Restate acceptance criteria and non-goals.
4. Inspect current tests before implementation.
5. Create the smallest test that demonstrates the missing behavior.

## Never assume

Do not assume OpenStack behavior, CellHV behavior, database schema, or provider semantics. Verify through committed contracts, public specifications, or executable tests.

## Preferred workflow

```text
issue -> spec clarification -> failing test -> minimal implementation
      -> refactor -> full checks -> evidence update -> PR
```

## High-risk changes

Stop for human review before:

- introducing `unsafe` or native FFI;
- changing a public API or protobuf field number;
- changing lifecycle states or recovery rules;
- adding a distributed coordination mechanism;
- changing license, provenance, cryptography, authentication, authorization, or secret storage;
- adding a provider that can destroy external resources;
- changing O3K–CellHV ownership boundaries.

## Expected final report

State what changed, what was tested, what remains uncertain, and which public sources informed compatibility behavior. Never provide hidden chain-of-thought; provide concise engineering rationale and evidence.
