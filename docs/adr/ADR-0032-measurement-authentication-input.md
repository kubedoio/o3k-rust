# ADR-0032 — Use the configured password in measurement authentication

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: identity, cli, governance

## Context

The measurement harness passes `O3K_BOOTSTRAP_PASSWORD` to the daemon but used
the literal `measurement-password` when requesting a token. A caller using a
custom password therefore measured a failed authentication path rather than
the configured service.

## Decision

Construct the JSON authentication request from the same password value passed
to `o3kd`, using JSON encoding so quotes and other password characters remain
data rather than shell syntax. The password is kept in the request body and is
not written to the measurement artifacts.

## Consequences

Custom-password fake measurements now exercise the configured authentication
path consistently. The harness still reports only control-plane measurements;
guest/libvirt measurements and release evidence remain separate requirements.
