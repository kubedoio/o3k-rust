# P12-IAM.6 — Public IAM Contracts, OpenAPI and Compatibility Policy

## Goal

Publish the completed P12-IAM northbound identity/federation surface as authoritative, versioned, machine-readable contracts consumable by Araf and other clients.

## Preconditions

- P12-IAM.5 merged.
- Start from latest protected `main`.

## Required contract coverage

Publish/update OpenAPI 3.1 and JSON Schemas for every public native identity route involved in the production flow, including as applicable:

- native token issuance/exchange;
- `/o3k/v1/identity/me`;
- federated scope discovery;
- rescoping/token exchange request and response bodies;
- RFC 9457 Problem Details;
- public-safe principal/scope projections;
- operator/system scope contract where public to authenticated clients.

Document:

- authentication requirements;
- headers/content types;
- request/response schema;
- stable machine error codes;
- token lifetime/expiry semantics;
- scope semantics;
- pagination where scope discovery is bounded;
- rate/size limits where part of the public contract;
- compatibility/versioning policy.

## Discovery configuration contract

Document how a confidential client discovers/configures the external OIDC provider versus O3K. O3K should not require the browser to learn private issuer configuration that is unnecessary for the client flow.

If O3K exposes public identity-provider metadata for clients, provide only the minimum safe configuration and keep secrets/private trust state server-side.

## Compatibility rules

Explicitly classify compatible vs breaking changes. Breaking examples include:

- changing external identity key semantics;
- changing scope meaning;
- removing a supported credential method;
- changing token request/response required fields;
- changing canonical error code meaning;
- broadening/narrowing system authorization semantics without version/profile transition.

Ensure existing native password/token and Keystone compatibility contracts remain represented correctly.

## Generated-client proof

Validate that standard OpenAPI tooling can resolve the published contract. Include at least one smoke generation/compile check without making one generator the architectural source of truth.

No unresolved `$ref`; all examples validate against schemas.

## Contract drift CI

Add/extend deterministic CI so public route/schema drift is detected. A semantic IAM route change must not silently bypass contract updates.

## Required security review

Check that public schemas/examples do not expose:

- raw access/native tokens;
- signing keys;
- JWKS cache internals;
- external provider client secrets;
- provider credentials;
- private assignment/policy details unnecessary to the caller.

## Completion evidence

List every public identity route, artifact path, schema version and compatibility rule. Run contract validation plus full Rust gates.

Verdict:

`P12-IAM.6 public contract gate: PASS|BLOCKED`
