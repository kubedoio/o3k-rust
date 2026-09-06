# P14.1 — Source Discovery and Preflight

Implement only bounded public-API discovery and preflight for the declared
source cloud. Bind the source to an opaque configured cloud ID and destination
scope using canonical IAM. Produce a redacted, content-addressed snapshot and
stable unsupported/capability failures. Do not create destination resources.

Test scope isolation, malformed/malicious source data, endpoint allowlists,
timeouts, credentials redaction, changed snapshots, and insufficient capacity.
