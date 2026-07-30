# TestLab workflow

`tests/testlab.sh` runs a repeatable local fake-provider acceptance workflow:

```text
token → image upload → network/subnet → flavor → server → process restart
→ durable show/list → server delete → subnet/network/image cleanup → reset
```

It uses only the public HTTP API, creates a temporary data directory, never
prints the subject token, and writes a machine-readable result plus daemon
logs under `target/testlab-artifacts` (or `O3K_TESTLAB_ARTIFACT_DIR`). The
standard OpenStack CLI can use the same process with `OS_AUTH_URL`,
`OS_USERNAME=admin`, `OS_PASSWORD=password`, `OS_PROJECT_NAME=admin`, and
`OS_USER_DOMAIN_NAME=Default`.

The default `O3K_TESTLAB_PROFILE=fake` is executable in CI. The `cellhv`
profile is reserved for an environment that supplies `CELLHV_ENDPOINT` and
credentials; it fails clearly when that external environment is absent rather
than silently testing the fake provider.
