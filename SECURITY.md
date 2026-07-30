# Security Policy

## Project status

O3K Rust is pre-alpha and must not be used for production workloads. Security boundaries and threat models are under active design.

## Reporting

Do not open a public issue for a suspected vulnerability that could expose credentials, tenant data, host access, provider access, or destructive operations. Report it privately to `security@kubedo.io` with:

- affected version or commit;
- reproduction steps;
- expected impact;
- suggested mitigation when available.

Do not include real secrets or customer data.

## Initial security principles

- least privilege and explicit tenant/project scope;
- no secret values in logs, traces, metrics, errors, or audit events;
- provider credentials isolated from public API handling;
- typed authorization decisions before mutation;
- persisted operation intent and idempotent recovery;
- bounded input sizes and strict schema validation;
- signed releases and SBOMs before public alpha;
- dependency, provenance, and license review;
- no `unsafe` without a dedicated ADR and human review.

## Supported versions

No version is currently supported for production security fixes. This policy will be updated before the first public alpha release.
