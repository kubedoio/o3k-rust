# Clean Implementation and Provenance Policy

## Statement

O3K Rust is a clean-slate Kubedo project. It is not a line-by-line port, translation, or derivative implementation of a non-public codebase.

## Allowed inputs

- public OpenStack API documentation and schemas;
- public OpenStack client, SDK, Terraform provider, and interoperability behavior;
- public standards such as SCS specifications;
- public source code used in compliance with its license and explicitly recorded;
- the public Apache-2.0 [Go O3K repository](https://github.com/kubedoio/o3k) as a
  non-normative requirements, inventory, failure-scenario, and black-box
  comparison reference;
- independently created black-box observations against publicly accessible software;
- Kubedo-authored product requirements, ADRs, experiments, and tests;
- general professional skills and publicly explainable operational lessons.

## Prohibited inputs

- non-public SAP, CobaltCore, NeoNephos, customer, partner, or employer source code;
- non-public architecture, roadmaps, schemas, tests, incidents, performance data, or internal discussions;
- mechanical translation of another implementation into Rust;
- treating the public Go O3K implementation as authoritative over an official
  OpenStack contract;
- reproducing the Go repository's monolithic architecture in Rust;
- code generated from private source through an LLM;
- unexplained code snippets with unknown origin or license.

## Provenance requirements

For each compatibility spec or substantial algorithm, record:

- public source URL or document identifier;
- access date when relevant;
- Kubedo design decision or experiment;
- author/agent and review PR;
- license for incorporated third-party source.

When Go O3K is inspected, also record the exact repository commit and every
relevant path (route, handler, test, fixture, or operational script) consulted.
If code, tests, or fixtures are copied or adapted, preserve Apache-2.0
copyright and NOTICE attribution and identify the changes. Requirements
discovered from Go must be independently expressed in Rust contracts and
black-box tests; no Go source or private implementation material is an
acceptable hidden input.

## Independent behavior tests

Black-box tests should be written from public contracts and observed public behavior. They must avoid using private test fixtures, internal database schemas, or hidden implementation structures.

## Repository separation

- separate repository and commit history;
- Kubedo-controlled accounts and development environments;
- no private remote or dependency;
- no copying of migrations, identifiers, comments, or directory structure from a non-public implementation;
- preserve agent prompt provenance without private data.

## Dependency policy

Every dependency must have:

- business/technical justification;
- compatible license;
- active maintenance or an ownership plan;
- bounded feature set;
- security advisory monitoring.

A future `cargo-deny` configuration will enforce accepted licenses and source policies.

## Review trigger

Any uncertainty about source origin, ownership, patent, trademark, or confidential information blocks merge until a human maintainer resolves it in writing.
