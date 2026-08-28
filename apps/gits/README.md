# Gits App

Gits is a small workspace-local Git host for Agents and humans. It stores bare
repositories, serves Git Smart HTTP through the installed `git http-backend`,
and exposes a compact repository index. It intentionally does not implement
issues, pull requests, forks, per-repository users, public ingress, or repository
deletion.

## Deploy

Run Gits as a Managed App so Treer owns its process, stable service, and virtual
hostname:

```sh
treer app create --machine self --name gits --cwd treer --port 9430 \
  --hostname gits.internal env -- \
  GITS_DATA_DIR=.treer/apps/gits \
  GITS_PUBLIC_URL=http://gits.internal \
  python3 apps/gits/gits.py
treer app show gits
```

Read `http://gits.internal/` for the Agent-facing Markdown manual. The human
repository index is at `http://gits.internal/_human/`.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `GITS_LISTEN` | `127.0.0.1:9430` | Private HTTP listener |
| `GITS_PUBLIC_URL` | `http://gits.internal` | Clone URL origin |
| `GITS_DATA_DIR` | `.treer/apps/gits` | Bare repository directory |
| `GITS_GIT_BIN` | `git` | Git executable with `http-backend` |
| `GITS_MAX_PUSH_BYTES` | `268435456` | Maximum Smart HTTP request body |

Any workspace Agent that can reach the virtual hostname can clone and push any
repository. Do not publish Gits through public ingress. Back up the complete
data directory while writes are stopped or with a filesystem snapshot that is
consistent across repository files.

## Test

```sh
python3 -m unittest discover -s apps/gits/tests -p 'test_*.py' -v
```
