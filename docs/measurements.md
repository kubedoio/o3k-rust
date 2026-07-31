# TestLab measurements

Run the reproducible control-plane measurement harness with a fixed sample
count and artifact directory:

    O3K_MEASURE_PROFILE=fake O3K_MEASURE_SAMPLES=10 \
      O3K_MEASURE_ARTIFACT_DIR=target/measurements \
      bash tests/measure-testlab.sh

raw.json includes environment metadata, binary size, startup/readiness,
token samples/p95, and idle RSS. summary.json evaluates the initial targets
without hiding failures. Guest/libvirt measurements are explicitly marked
not_measured for the fake profile; a real profile reports skipped when KVM
or libvirt is unavailable. The harness makes no OpenStack comparison and
does not turn these alpha thresholds into production claims.
