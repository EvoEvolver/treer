# Self-hosted Compose and control-plane updates

Self-hosted Treer runs PostgreSQL, NATS, Proxy, the browser App, and an updater
sidecar from published GHCR images. This is the operator path for a machine you
control. The hosted Railway plus Cloudflare path is unchanged and does not
start this sidecar.

## Images and tags

CI publishes three images when a `v*` git tag is pushed. Push that tag only
after Canary has passed for the same commit, and only when the tag matches the
workspace version in `Cargo.toml` (`v0.1.3` for version `0.1.3`).

| Image | Role |
| --- | --- |
| `ghcr.io/evoevolver/treer-proxy` | Proxy plus bundled Linux machine artifacts |
| `ghcr.io/evoevolver/treer-app` | Nginx static App with runtime `config.json` |
| `ghcr.io/evoevolver/treer-updater` | Compose updater; the only process that mounts `docker.sock` |

Immutable tags never move:

- `vX.Y.Z` from the git tag
- `sha-<full commit>`

Channel pointers may move, like R2 artifact channels:

- `canary` moves when a `v*` tag is published
- `stable` moves only through the `Publish GHCR images` workflow dispatch after
  Production promotion, or when an operator chooses that channel for self-host

`latest` is not a Treer channel. The updater rejects it.

Forks set `TREER_GHCR_OWNER` to the lowercase GitHub owner that owns the
packages. Actions-published GHCR packages start private; GitHub does not let
`GITHUB_TOKEN` change that visibility. An organization owner must set each
package to Public before anonymous `docker compose pull` works:

- [treer-proxy](https://github.com/orgs/EvoEvolver/packages/container/package/treer-proxy)
- [treer-app](https://github.com/orgs/EvoEvolver/packages/container/package/treer-app)
- [treer-updater](https://github.com/orgs/EvoEvolver/packages/container/package/treer-updater)

For private packages, set `TREER_GHCR_TOKEN` to a read token.

## First start

Copy `.env.example` to `.env`, set `ADMIN_PASSWORD`, `POSTGRES_PASSWORD`,
`DATABASE_URL`, and a long random `TREER_UPDATER_TOKEN`. Point
`TREER_PROXY_PUBLIC_URL` and `TREER_APP_PUBLIC_URL` at the URLs browsers and
machines will actually use.

```bash
cp .env.example .env
docker compose pull
docker compose up -d
```

Default `TREER_IMAGE_TAG=stable`. Until a `stable` pointer exists, set
`TREER_IMAGE_TAG=canary` or a version tag such as `v0.1.3`.

The updater listens only on the Compose network. It does not publish a host
port. Proxy talks to it at `http://updater:7420` with the shared token.

Local source builds use the overlay instead of GHCR:

```bash
docker compose -f compose.yaml -f compose.dev.yaml up --build
```

`/admin` update still compares the configured GHCR channel, not the `:dev`
images, so do not use Apply against a source-build stack unless that is
intentional.

## Updating the control plane

Open `/admin` as the platform administrator, not workspace Settings. Check for
updates, then Apply. Proxy forwards those calls to the sidecar; Proxy never
mounts the Docker socket.

Apply pulls `proxy`, `app`, and `updater` for the configured channel, recreates
Proxy and App, then recreates the updater. Recreating the updater can drop the
in-flight HTTP call. The admin UI polls until the new digest is running or the
job reports failure.

Roll back by setting `TREER_IMAGE_TAG` to a previous `vX.Y.Z` or `sha-<commit>`
and running `docker compose up -d`. Channel tags are convenience pointers, not
the rollback record.

## Updating enrolled machines

Control-plane Apply does not roll Controllers. On each enrolled machine, keep
using:

```bash
treer-agent-server update
```

Remote orchestration of machine updates from `/admin` is a follow-up.

## Security boundary

- `treer-proxy` has no Docker socket
- the updater has `docker.sock` and a read-only bind of `compose.yaml`
- `/api/admin/update*` require the platform admin session
- hosted Railway leaves `TREER_UPDATER_URL` unset; `/admin` then shows that
  updates are not configured
