# Treer script plugins

- Status: maintained
- Manifest schema: `plugin-v1`

Treer plugins are executable scripts that translate an external surface into
Core commands. Their only supported Treer integration is the installed `treer`
CLI. A first-party plugin must not link a Treer Rust crate, call Proxy or
Controller routes, connect to Treer's PostgreSQL database, or consume Core NATS.

## Runtime contract

`treer plugin install` validates a package and installs one immutable version.
Installation never executes package code. `treer plugin run` starts the selected
version with a private Unix broker and a cleared environment. The script gets:

| Variable | Value |
| --- | --- |
| `TREER_CLI` | Exact CLI executable to invoke for nested commands |
| `TREER_PLUGIN_CONFIG` | Operator-owned JSON configuration path |
| `TREER_PLUGIN_STATE_DIR` | Private, versioned plugin state directory |
| `TREER_PLUGIN_ID` / `TREER_PLUGIN_VERSION` | Selected package identity |
| `TREER_PLUGIN_BROKER_SOCKET` / `TREER_PLUGIN_BROKER_TOKEN` | Opaque broker transport used by nested CLI invocations |

Only manifest-declared configuration and secret variables are copied into the
process. The runner does not pass raw workload, operator, machine, or enrollment
credentials. The nested CLI submits a semantic command to the broker; the
broker rejects undeclared commands before any network request, then ordinary
workspace Policy authorizes the request.

This is a command capability boundary, not a hostile same-UID sandbox. A script
running as the same operating-system user may still inspect accessible files or
other processes. Use a separate user, container, or stronger runtime when plugin
code is not trusted.

## Package shape

Every package has a strict `plugin.json` matching
[`plugin-v1.schema.json`](schema/plugin-v1.schema.json), one script entrypoint,
and optional `config.schema.json`, documentation, static assets, migration tools,
and tests. Packages containing Rust source, `Cargo.toml`, symlinks, unsafe paths,
too many files, or excessive bytes fail validation.

```sh
treer plugin validate plugins/mail
treer plugin install plugins/mail
treer plugin list
treer plugin inspect mail
treer plugin run mail --config /etc/treer/mail.json
```

Installed versions are read-only and selected by highest semantic version.
Plugin-owned state is scoped by workspace, plugin ID, and version. Uninstall and
automatic state migration are intentionally absent in v1; back up state before
changing versions and revoke plugin human sessions when retiring an instance.

## Development rules

- Use stdin for Message bodies so content does not appear in process arguments.
- Use stable idempotency keys before retrying an external update.
- Persist an external side effect or mapping before acknowledging its Core
  delivery.
- Treat names and channel usernames as display values; authorize with stable
  numeric or Core IDs.
- Keep channel tokens and retry/mapping databases plugin-owned.
- Test with a fake `treer` executable. Such tests must run without importing
  repository internals or starting a private Proxy route.
- Do not log Message bodies, broker tokens, human capabilities, OAuth codes, or
  channel secrets.

The first shipped packages are [Mail](mail/README.md) and
[Telegram](telegram/README.md). Additional channel adapters use the same Core
Message and broker contract.
