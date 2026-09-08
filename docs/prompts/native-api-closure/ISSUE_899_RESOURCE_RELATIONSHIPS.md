# Implementation Prompt — Issue #899

## Mission

Implement **#899 — Expose canonical resource relationships safely and generically** on this branch.

## Before coding

Refresh from current `main`, read issue #899 fully, and inspect canonical relationship records/repositories for SQLite/PostgreSQL, composition protocol/state machine, volume attachment/router-interface evidence, generic native resource authorization and every provider-private binding that must remain hidden.

The public contract must project existing O3K relationship authority; do not create an Araf topology model or infer relationships from names/IPs/device fields.

## Required implementation

Create the smallest versioned generic native relationship read contract that:

- is anchored to canonical O3K resources/relationship records;
- exposes only safe canonical source/slot/target/state/ownership/timestamp/generation/correlation fields that are truly authoritative;
- independently authorizes the source and prevents foreign target disclosure;
- does not make a visible relationship into authorization delegation;
- supports explicit system/operator cross-scope diagnostics only through canonical system authorization;
- preserves reserved/bound/deleting/deleted/unknown semantics rather than flattening uncertainty;
- performs bounded repository queries with strict page limits/cursors/indexes where needed;
- never exposes provider IDs, host/device paths, hypervisor targets, Ceph/storage credentials or controller-private fields.

Add ADR/SPEC/public schema artifacts and drift tests if no accepted relationship API exists.

## Required evidence

Use real canonical workflows to prove at least:

1. server-volume attachment relationship;
2. router-interface or another existing network relationship;
3. Project A cannot enumerate Project B relationships or infer hidden targets;
4. target reads remain separately authorized;
5. restart preserves relationship state;
6. reserved -> bound and delete/deleting transitions are truthful;
7. child replacement/delete-recreate produces the correct distinct canonical identity;
8. compensation/recovery behavior remains visible correctly;
9. SQLite/PostgreSQL parity;
10. collection queries are bounded/indexed;
11. public JSON/errors contain no provider-private data.

## Convergence

Do not derive native relationship truth from compatibility tables if the canonical relationship repository exists. Where OpenStack/native workflows create the same canonical relationship, add convergence evidence that both surfaces observe the same durable relationship authority.

## Validation

Run all current-main required Rust gates, store conformance, relevant ignored PostgreSQL tests, relationship recovery/concurrency/process tests, native API two-tenant negatives and compatibility convergence gates.

## Forbidden shortcuts

Do not add a browser-generated graph as production authority. Do not expose raw relationship database rows wholesale. Do not turn relationship visibility into permission to read/mutate the child. Do not materialize global relationship collections and filter in memory.

## Stop condition

Only report `BLOCKED` for a genuine dependency outside this repository. Cross-crate API/store/index/spec changes belong in this PR.

Finish only with `#899 COMPLETE` when the issue exit criteria are proven.