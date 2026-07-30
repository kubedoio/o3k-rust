# O3K configuration

`o3kd` validates its complete configuration before opening a listening socket.
The default profile is local and safe: it listens on `127.0.0.1:8080`, uses
`./data`, JSON logs at the `info` level, and the stateful `fake` provider.

## Precedence

Values are merged in this order, from lowest to highest priority:

1. built-in defaults;
2. the optional TOML file selected by `--config` or `O3K_CONFIG`;
3. `O3K_*` environment variables;
4. command-line flags.

The `--config` flag takes precedence over `O3K_CONFIG` for selecting the file.
Unknown TOML keys and command-line options are rejected. Errors identify the
problem without printing secret values.

## Fields

| Setting | TOML key | Environment | CLI flag | Default |
| --- | --- | --- | --- | --- |
| Listen address | `listen_addr` | `O3K_LISTEN_ADDR` | `--listen-addr` | `127.0.0.1:8080` |
| Data directory | `data_dir` | `O3K_DATA_DIR` | `--data-dir` | `./data` |
| Log format | `log_format` | `O3K_LOG_FORMAT` | `--log-format` | `json` |
| Log filter | `log_filter` | `O3K_LOG_FILTER` | `--log-filter` | `info` |
| Provider | `provider` | `O3K_PROVIDER` | `--provider` | `fake` |
| Bootstrap secret | `bootstrap_secret` | `O3K_BOOTSTRAP_SECRET` | `--bootstrap-secret` | unset |

`log_format` accepts `json` or `pretty`; `provider` accepts `fake` or `cellhv`.
The default address must remain loopback unless an operator explicitly changes
it. Secrets have redacted `Debug` and `Display` representations and must not be
included in logs, errors, or command output.

Example:

```toml
listen_addr = "127.0.0.1:8080"
data_dir = "/var/lib/o3k"
log_format = "json"
log_filter = "info,o3k=debug"
provider = "fake"
```

Readiness is distinct from liveness: `/healthz` reports that the process is
alive, while `/readyz` returns `503` until startup completes and `200` only
when the process can accept requests.
