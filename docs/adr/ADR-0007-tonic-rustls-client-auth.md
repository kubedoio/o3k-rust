# ADR-0007 — Tonic version for compute-agent mutual TLS

Status: Accepted

## Context

Issue #38 requires the compute agent to establish a mutually authenticated
gRPC stream. The first implementation used tonic 0.12.3, the workspace's
existing version. Its configured client identity was accepted while building
the endpoint, but rustls did not select a client certificate when the server
sent `CertificateRequest`; the server consequently rejected the handshake
with `UnknownCA`.

The same CA, certificate chain, and private key were validated independently
with a direct rustls handshake. This isolated the failure to tonic's TLS
integration rather than certificate material or the registration protocol.

## Decision

Use tonic 0.13.1 and tonic-build 0.13.1 for the workspace, with tonic's
explicit `tls-ring` provider feature. This release contains the rustls
PEM-to-`rustls-pki-types` conversion fix needed for client certificate
selection; the explicit provider feature is required because tonic 0.13 no
longer selects a crypto provider implicitly. Keep the TLS integration as a
black-box regression test that proves server authentication, client
authentication, registration, and heartbeat observability together.

The control plane also requires an explicit authorized-agent mapping in
`id=sha256(certificate DER)` form. A trusted client CA alone is not an
enrollment record: the certificate URI SAN must match the requested agent ID
and the leaf certificate SHA-256 must match the configured mapping.

## Evidence

- Direct rustls handshake with the repository fixtures passed before the
  upgrade.
- The same tonic 0.12.3 test repeatedly logged `Client auth requested but no
  cert/sigscheme available` and failed with `UnknownCA`.
- The unchanged compute-agent test passed with tonic 0.13.1 and logged
  `Attempting client auth` followed by successful registration and heartbeat.
- The negative integration test leaves the registry empty when the client CA
  is not trusted by the control plane.

## Consequences

- The dependency review records the reason for the tonic upgrade and no
  longer needs the old `rustls-pemfile` advisory exception.
- The workspace remains on the Rust 1.85-compatible tonic 0.13 line; tonic
  0.14 is not usable here because it currently requires Rust 1.88.
- Future tonic upgrades must rerun the mTLS regression and review the
  dependency/advisory graph.

## Provenance

This is a Kubedo-authored compatibility experiment based on the public tonic
and rustls APIs. No private implementation or source was used.
