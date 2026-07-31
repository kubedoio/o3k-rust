# TestLab measurements

Run the reproducible control-plane measurement harness with a fixed sample
count and artifact directory:

    O3K_MEASURE_PROFILE=fake O3K_MEASURE_SAMPLES=10 \
      O3K_MEASURE_ARTIFACT_DIR=target/measurements \
      bash tests/measure-testlab.sh

raw.json includes environment metadata, binary size, startup/readiness,
token samples/p95, and idle RSS. summary.json evaluates the initial targets
without hiding failures and carries `raw_sha256`, a SHA-256 digest of the
canonical raw JSON. The digest is calculated after `raw.json` is written,
using sorted keys, compact separators, UTF-8, and escaped non-ASCII
characters, matching the release gate.

Fake-profile artifacts are explicitly marked `release_eligible: false` with
an exclusion reason: they are measurement/diagnostic evidence, not libvirt
release evidence. Guest/libvirt measurements are marked `not_measured` for
the fake profile; a real profile reports skipped when KVM or libvirt is
unavailable. The harness makes no OpenStack comparison and does not turn
these alpha thresholds into production claims.
