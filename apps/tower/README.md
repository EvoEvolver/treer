# TOWER App

TOWER is an experimental workspace App implementing the evidence archive at the
base of tiered Agent oversight. The first version captures ACP JSON-RPC frames,
stores them as a content-addressed prefix DAG, and accepts immutable reviewer
findings linked to exact source events. It does not capture hidden chain of
thought or independently prove external tool execution.

## Deploy

Run TOWER as a private Managed App:

```sh
TOKEN="$(openssl rand -hex 32)"
treer app create --machine self --name tower --cwd treer --port 9460 \
  --hostname tower.internal env -- \
  TOWER_DATA_DIR=.treer/apps/tower \
  TOWER_TOKEN="$TOKEN" \
  python3 apps/tower/tower.py
treer app show tower
```

Keep the token outside Agent-visible files when the deployment environment can
provide a secret store. Do not use `--public`: traces may contain source code,
prompts, paths, tool output, and credentials.

Configure an ACP launcher Agent with:

```sh
TOWER_URL=http://tower.internal
TOWER_TOKEN="$TOKEN"
```

The launcher remains unchanged when `TOWER_URL` is absent. When enabled it
spools every parsed ACP frame to `tower-spool.sqlite` beside the existing ACP
journal, then uploads ordered batches in the background. Archive outages do not
block the Agent; pending events retry after restart.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `TOWER_LISTEN` | `127.0.0.1:9460` | App HTTP listener |
| `TOWER_DATA_DIR` | `.treer/apps/tower` | SQLite archive directory |
| `TOWER_TOKEN` | unset | Optional bearer token required by mutating `/v1` routes |

## Storage And Backup

The SQLite database separates blobs, prefix nodes, ordered stream events, and
review findings. Prefix nodes and blobs use deterministic SHA-256 identities;
event IDs make retries idempotent. Back up the complete data directory with a
filesystem snapshot or while writes are stopped.

This MVP assumes the App and its host are trusted. ACP capture proves what the
launcher observed relative to the provider process, not resistance to a
same-user machine compromise. Future versions should move collection outside
the Agent account, sign hash-chained segments, add trusted execution receipts,
and enforce capture through Treer Policy.

## Test

```sh
python3 -m unittest discover -s apps/tower/tests -p 'test_*.py' -v
```
