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
  env -- \
  GITS_DATA_DIR=.treer/apps/gits \
  GITS_PUBLIC_URL=http://gits.internal \
  python3 apps/gits/gits.py
treer app show /gits
```

Read `http://gits.internal/` with an Agent client for the Markdown manual, or
open the same URL in a browser for the repository index.

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

## Local state isolation

Use a dedicated private state directory; startup tightens its permissions to
0700. Python stores also use a private umask and private database files. Do not
use a shared HOME/project directory as state. Backups remain plaintext and need
restrictive permissions/encryption. This does not isolate same-UID processes.
For critical services use a separate unprivileged user or container/VM with
minimal mounts and resource limits. Workspace ingress is not per-user data ACL
or comprehensive request-rate protection; retain this App's documented trusted
workspace assumptions and do not make it public.
