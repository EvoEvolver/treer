# Soul server

Soul is an experimental workspace-local file service. It stores an immutable
tar bundle whose `manifest.json` maps environment variable names to files, and
can create a command Agent that downloads the bundle before executing a
command. Deploy it as a Managed App so Treer persists and supervises its
process, service, and virtual host. Treer does not authenticate or interpret
Soul data.

The service is intended for trusted workspaces. Any Agent that can reach the
virtual host can upload or download every Soul and can ask the service Agent to
create another Agent with the service Agent's Treer authority. Do not expose it
through public ingress.

## Start and register

Deploy the server from a managed Agent. The dedicated App runtime receives its
own workload identity and private sandbox; Core owns the service and virtual
host instead of allowing the calling Agent to register its own listener:

```bash
treer app create --machine self --name soul --cwd . --port 9420 \
  --hostname soul.internal env -- \
  SOUL_DATA_DIR=.treer/apps/soul \
  SOUL_PUBLIC_URL=http://soul.internal \
  python3 apps/soul/soul.py
treer app show soul
```

Install the client from any managed Agent in that workspace:

```bash
curl -fsSL http://soul.internal/install.sh | sh
```

This installs `treer-soul` under `~/.local/bin` and its Python implementation
under `~/.local/libexec/treer-soul`. Override `TREER_SOUL_INSTALL_DIR` or
`TREER_SOUL_LIBEXEC_DIR` when required. The installer verifies the downloaded
client's SHA-256 digest.

Read `http://soul.internal/` for the Agent-facing plain-text usage guide. Open
`http://soul.internal/_human/` to view stored Souls, manifests, environment
bindings, files, archive sizes, and checksums. The human page is read-only;
uploads and incarnations remain CLI/API operations.

## Generic souls

An archive contains `manifest.json` and regular files. Paths must be relative,
must not contain `..`, and must name files present in the archive. Runtime and
loader variables such as `PATH`, `LD_PRELOAD`, `CODEX_HOME`, and `TREER_*` are
reserved and cannot be supplied by a Soul:

```json
{
  "schema_version": 1,
  "name": "Example state",
  "environment": {
    "AGENT_TRACE_PATH": "files/trace.jsonl",
    "AGENT_STATE_PATH": "files/state.json"
  }
}
```

Upload it with explicit archive-to-local mappings:

```bash
treer-soul upload --manifest manifest.json \
  --file files/trace.jsonl=./trace.jsonl \
  --file files/state.json=./state.json
```

Create an Agent and run a command with those environment variables:

```bash
treer-soul incarnate soul_ID --machine self --name state-reader --cwd treer -- \
  sh -c 'wc -l "$AGENT_TRACE_PATH"; exec "$SHELL" -i'
```

The new Agent downloads and extracts the bundle under
`~/.local/state/treer-soul/incarnations/`, exports the mapped absolute paths,
sets `TREER_SOUL_ID` and `TREER_SOUL_ROOT`, and executes the requested argv.

## Codex souls

Codex CLI 0.149.0 records a resumable local session as one rollout JSONL under
`$CODEX_HOME/sessions/YYYY/MM/DD/`. Its first `session_meta` record contains the
session UUID, saved working directory, originator, and CLI version. A matching
file under `$CODEX_HOME/shell_snapshots/` may capture shell initialization, but
the supported recovery operation is the session UUID passed to `codex resume`.

Capture the current Codex UI thread, or name a session explicitly:

```bash
treer-soul capture-codex --name current-codex
treer-soul capture-codex --session 01234567-89ab-cdef-0123-456789abcdef
treer-soul capture-codex --session 01234567-89ab-cdef-0123-456789abcdef \
  --include-shell-snapshot
```

The capture contains the rollout. A shell snapshot is included only when
explicitly requested because it can contain sensitive environment data. The
adapter deliberately excludes `auth.json`, `config.toml`, memories, logs,
plugins, and credentials. Rollouts contain prompts and tool output and can also
contain secrets that appeared in the session, so protect the Soul data
directory accordingly. The target machine must already have a compatible
Codex CLI and its own valid authentication.

For a Codex Soul, the command is optional:

```bash
treer-soul incarnate soul_ID --machine build-machine --name codex-reborn --cwd treer
```

The launcher restores the rollout below the target `$CODEX_HOME/sessions`, then
executes:

```bash
codex resume SESSION_ID --dangerously-bypass-approvals-and-sandbox -C "$PWD"
```

Codex documents `codex resume SESSION_ID` as stable, but its rollout JSONL and
shell-snapshot layouts are internal implementation details. Treat transfer
between different Codex versions as best effort. Do not resume the same session
concurrently from two running Agents.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `SOUL_LISTEN` | `127.0.0.1:9420` | Private HTTP listener |
| `SOUL_PUBLIC_URL` | `http://soul.internal` | URL embedded in installers and launchers |
| `SOUL_DATA_DIR` | `.treer/apps/soul` | Server archive and metadata directory |
| `SOUL_MAX_UPLOAD_BYTES` | `67108864` | Maximum tar upload size |
| `SOUL_CLIENT_PATH` | sibling `client.py` | Client served by `/client.py` |
| `TREER_BIN` | `treer` | CLI used by the incarnation endpoint |

## Test

```bash
python3 -m unittest discover -s apps/soul/tests -p 'test_*.py' -v
```
