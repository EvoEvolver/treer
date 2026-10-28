# Gits App

Gits is a small workspace-local Git host for Agents and humans. It stores bare
repositories, serves Git Smart HTTP through the installed `git http-backend`,
and exposes a compact repository index. It does not implement issues, pull
requests, forks, per-repository users, public ingress or repository deletion.

## Deploy

Run Gits as a Managed App so Treer owns its process and stable service:

```sh
treer app create --machine self --name gits --cwd treer --port 9430 \
  env -- \
  GITS_DATA_DIR=.treer/apps/gits \
  GITS_PUBLIC_URL=http://gits.treer \
  python3 apps/gits/gits.py
treer app show /gits
```

The App's service takes the App name, so Agents reach it through their
inherited workload proxy at `http://gits.treer/` (or `http://SERVICE_ID.treer/`
from `treer service list`; a suffix is added if the name is already taken).
Set `GITS_PUBLIC_URL` to that address so clone URLs in the index are usable.
`curl http://gits.treer/` returns the Markdown manual ([AGENT.md](AGENT.md));
a browser gets the repository index.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `GITS_LISTEN` | `127.0.0.1:9430` | Private HTTP listener (match `--port`) |
| `GITS_PUBLIC_URL` | `http://gits.internal` | Clone URL origin shown in the index; set it explicitly |
| `GITS_DATA_DIR` | `.treer/apps/gits` | Bare repository directory |
| `GITS_GIT_BIN` | `git` | Git executable with `http-backend` |
| `GITS_MAX_PUSH_BYTES` | `268435456` | Maximum Smart HTTP request body |

## API

- `GET /health`
- `GET /v1/repos`, `GET /v1/repos/NAME`, `POST /v1/repos` (create)
- `/git/NAME.git/...` Git Smart HTTP (clone, fetch, push)

## Access, storage and backup

Any workspace Agent that can reach the service can clone and push any
repository. Do not publish Gits with `--public`. Back up the complete data
directory while writes are stopped or with a filesystem snapshot that is
consistent across repository files.

Use a dedicated private state directory; startup tightens it to 0700. Do not use
a shared HOME/project directory as state. Backups are plaintext and need
restrictive permissions or encryption. Same-UID processes are not isolated; on
critical-service machines run Gits under a separate unprivileged user or
container/VM.

## Test

```sh
python3 -m unittest discover -s apps/gits/tests -p 'test_*.py' -v
```
