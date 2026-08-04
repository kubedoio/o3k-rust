# ADR-0143 — Fence console storage and bounded reads

Status: accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: storage, governance

The console root must be a real directory and is restricted to mode `0700` on
Unix. Published console artifacts and temporary files are regular private
files; symlinked or non-regular artifacts are rejected. Temporary writes use
exclusive creation with mode `0600` before atomic publication.

Reads reject oversized artifacts before loading their bytes, and append/chunk
paths propagate storage errors instead of treating permission or corruption
failures as an empty console. This preserves the bounded durable-cache
contract for issue #84.

Live nonzero-offset agent queries and real guest console evidence remain
host/agent integration requirements.
