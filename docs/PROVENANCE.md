# Provenance record format

Every release candidate or material AI-assisted change may include a small,
public provenance record. It contains metadata only—never credentials,
private prompts, customer data, internal documents, or source code copied from
non-public systems.

```json
{
  "source_commit": "<full git commit>",
  "workflow": "<local or GitHub Actions workflow name>",
  "workflow_run": "<public run id or null>",
  "builder": "<tool and version>",
  "inputs": ["<public repository path or release input>"],
  "checks": ["cargo fmt", "cargo clippy", "cargo test", "cargo deny"],
  "artifacts": ["SHA256SUMS", "sbom.spdx.json"],
  "ai_assistance": "<summary of material assistance, or none>",
  "secrets_included": false
}
```

The final field is a required assertion. Do not add fields containing secret
values, private file paths, access tokens, customer identifiers, or hidden
system instructions.

For compatibility work, `inputs` must distinguish normative OpenStack sources
from non-normative references. When the public Go O3K repository is consulted,
record its exact commit and inspected paths in the issue or pull request. If
code, tests, or fixtures are adapted, record the source path, Apache-2.0
copyright/NOTICE attribution, and the change; otherwise record `none reused`.
