# TOWER App

TOWER is an experimental append-only evidence archive for Agent traces. It
stores JSON-RPC (ACP-shaped) frames as a content-addressed prefix DAG and
accepts immutable reviewer findings linked to exact source events. It does not
capture hidden chain of thought or prove external tool execution.

No bundled launcher sends traces to TOWER; producers post batches to
`/v1/ingest` themselves. The Agent manual is [AGENT.md](AGENT.md).

## Deploy

Run TOWER as a private Managed App:

```sh
TOKEN="$(openssl rand -hex 32)"
treer app create --machine self --name tower --cwd treer --port 9460 \
  env -- \
  TOWER_DATA_DIR=.treer/apps/tower \
  TOWER_TOKEN="$TOKEN" \
  python3 apps/tower/tower.py
treer app show /tower
```

Agents reach it at `http://tower.treer/` through their workload proxy. Keep the
token outside Agent-visible files where a secret store is available. Do not use
`--public`: traces may contain source code, prompts, paths, tool output and
credentials.

| Variable / flag | Default | Meaning |
| --- | --- | --- |
| `TOWER_LISTEN` / `--listen` | `127.0.0.1:9460` | HTTP listener |
| `TOWER_DATA_DIR` / `--data` | `.treer/apps/tower` | SQLite archive directory |
| `TOWER_TOKEN` | unset | Bearer token required by mutating `/v1` routes when set |

## API

- `GET /health`, `GET /v1/stats`
- `GET /v1/streams`, `GET /v1/streams/ID/events`, `GET /v1/events/ID`
- `GET /v1/findings`, `POST /v1/findings`
- `POST /v1/ingest` (idempotent; at most 256 events and 4 MiB per batch)

Read routes rely on workspace ingress; writes need the bearer token when
configured.

## Storage and backup

The SQLite database separates blobs, prefix nodes, ordered stream events and
review findings. Blobs and prefix nodes use SHA-256 identities; event IDs make
retries idempotent. Back up the complete data directory with a filesystem
snapshot or while writes are stopped.

This MVP trusts the App and its host: an archived trace proves what the producer
reported, not resistance to a same-user compromise. Startup tightens the state
directory to 0700; backups are plaintext. Run under a separate unprivileged user
or container/VM on critical-service machines.

## Test

```sh
python3 -m unittest discover -s apps/tower/tests -p 'test_*.py' -v
```
