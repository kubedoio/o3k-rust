# LLM-First Development Process

## Principle

LLM agents are implementation workers operating inside explicit engineering controls. They may draft code, tests, docs, and issue plans. Humans retain responsibility for product scope, architecture, security, licensing, public contracts, releases, and acceptance.

## Agent roles

### Spec agent

- converts an approved product requirement into a normative spec;
- identifies public sources and uncertainties;
- writes acceptance criteria and negative cases;
- does not implement code in the same step.

### Implementation agent

- works from one approved issue and spec;
- adds failing tests;
- implements the smallest change;
- reports deviations and uncertainties.

### Test agent

- designs black-box, failure, property, and regression tests;
- avoids tests that merely mirror implementation structure;
- maintains compatibility evidence.

### Review agent

- checks issue scope, source provenance, contracts, state transitions, security, error handling, and tests;
- cannot approve its own implementation without human review for high-risk areas.

### Documentation agent

- updates user/operator documentation from accepted behavior and test evidence;
- never upgrades claims beyond measured results.

## Required issue fields

Every implementation issue must contain:

- context and user outcome;
- normative spec links;
- public source links;
- in-scope and out-of-scope behavior;
- acceptance criteria;
- expected tests;
- observability requirements;
- security and recovery considerations;
- provenance constraints.

## Prompt discipline

A coding prompt should include only:

- repository and issue;
- relevant committed specs/contracts;
- relevant public sources;
- exact acceptance criteria;
- commands to validate.

Do not paste private code or internal documents. Do not ask an agent to “implement OpenStack behavior” without naming the operation, version, fields, and tests.

## Context minimization

Agents should read the minimum necessary code after mandatory project documents. Large-context ingestion increases accidental coupling and weakens review. Prefer named files, interfaces, and tests.

## Evidence record

PRs must record:

- AI system/model when known;
- material prompts or task instructions;
- public sources used;
- tests executed;
- human reviewer decisions for architecture or security.

Do not store secrets or private content in prompt logs.

## Agent quality gates

An agent task is rejected when it:

- changes behavior without a spec/test;
- invents compatibility claims;
- copies non-public implementation details;
- adds a dependency without review;
- hides an error with retries or broad exception handling;
- uses `unwrap`, panic, or placeholder success in a production path;
- creates a fake provider that cannot model failure states;
- changes unrelated files;
- leaves undocumented generated artifacts.

## Recommended development loop

1. human approves issue and scope;
2. spec agent drafts or refines spec;
3. human accepts spec;
4. test agent adds executable expectations;
5. implementation agent makes them pass;
6. review agent checks architecture and failure behavior;
7. human reviews high-risk areas;
8. CI produces evidence;
9. documentation/compatibility matrix is updated;
10. PR merges.
