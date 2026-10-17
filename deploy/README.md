# Self-hosted Treer

Self-hosted Treer runs PostgreSQL, NATS, Proxy, the browser App, and an updater
sidecar from published GHCR images. This is the operator path for a machine you
control. The hosted Railway plus Cloudflare path is unchanged and does not
start this sidecar.

## Single container

The single-container **central server** image is `ghcr.io/evoevolver/treer`.
It contains PostgreSQL 17, the standalone Proxy, the browser Console, and native
Linux machine binaries built from the same commit. Machines, Controllers,
Hosts, Agent runtimes and workspace Apps run separately and enroll normally;
they are not started inside this container. The Compose installation below is
still supported.

Use a published version or `sha-<full commit>` tag (substitute `vX.Y.Z` below).
There is no `latest` channel. First create a named volume and start:

```bash
docker run -d --name treer --restart unless-stopped --stop-timeout 65 \
  --read-only --tmpfs /tmp:rw,nosuid,nodev \
  --cap-drop ALL --cap-add CHOWN --cap-add DAC_OVERRIDE \
  --cap-add SETUID --cap-add SETGID --cap-add KILL \
  --security-opt no-new-privileges:true \
  -p 127.0.0.1:8080:8080 --mount type=volume,src=treer-data,dst=/data \
  ghcr.io/evoevolver/treer:vX.Y.Z
```

Docker creates the named volume if absent. Open the Console at
`http://localhost:8080`; its API is `http://127.0.0.1:8080`. These are two
different browser origins on **one port**, without DNS setup. Preserve that
separation: the Proxy serves Console files only for the configured Console
Host, API/embedded UIs only for the Proxy Host, and rejects other Hosts.
The local addresses work only on the server machine, not remote clients.

When `ADMIN_PASSWORD` / `ADMIN_PASSWORD_FILE` is absent, a random platform
administrator password is generated once. Retrieve it deliberately in a
private terminal (this command displays the secret):

```bash
docker exec treer cat /data/private/admin-password
```

Use it at `http://localhost:8080/admin` to create an invitation, then register
your ordinary user account and create a workspace. The platform admin password
is a **long-lived startup credential**, not the ordinary member password. Do
not delete its file and expect a restart to work. Rotation: mount a private,
root-owned `0600` file, set `ADMIN_PASSWORD_FILE` to its container path and
recreate the container; remove the old generated file only after verifying the
new configuration. Subsequent starts must keep supplying the new file. Existing
sessions are not claimed to be revoked by password rotation.

For public deployment, put TLS termination in front of this port and configure
two HTTPS names pointing to it, for example:

```text
TREER_APP_PUBLIC_URL=https://console.example.com/
TREER_PROXY_PUBLIC_URL=https://proxy.example.com/
```

The reverse proxy must preserve the original `Host` and support WebSockets;
its certificate must cover both names. Do not expose plain HTTP publicly.
Cookie/Origin/CSRF checks remain enabled. Identical origins cannot safely embed
untrusted App/Agent UIs: the Core single-origin mode warns and disables framing;
use two names for the full Console. Wildcard service ingress additionally needs
its own DNS/TLS domain via `TREER_INGRESS_PUBLIC_URL`. Do not route arbitrary
unknown Hosts to the Console. Configure routable public URLs before enrolling
remote machines; loopback defaults will not connect from another machine.

Configuration is through environment variables (no manual config editing):

| Variable | Default / behavior |
| --- | --- |
| `ADMIN_PASSWORD`, `ADMIN_PASSWORD_FILE` | Env takes precedence; otherwise private file, otherwise generated on first start |
| `POSTGRES_PASSWORD`, `POSTGRES_PASSWORD_FILE` | Random on first start; persisted, mismatched replacement rejected (not a DB rotation API) |
| `TREER_PROXY_PUBLIC_URL` | `http://127.0.0.1:8080/` |
| `TREER_APP_PUBLIC_URL` | `http://localhost:8080/` |
| `TREER_ENABLE_CORE_MESSAGES` | `true`, works without NATS |
| `TREER_NATS_URL` | Unset; external NATS is opt-in, none is installed |
| `TREER_SINGLE_MAINTENANCE` | Unset; `restore` starts only PG for an offline logical restore |
| Other Proxy OAuth, invitation, audit and ingress variables | Same as the normal Proxy |

Explicit empty secrets, unsafe files and partial initialization fail closed.
`*_FILE` files must be regular, single-link, root-owned, private (`0600`), and
contain one nonempty UTF-8 line. Secrets supplied directly as environment
variables are visible to Docker administrators through `docker inspect`;
prefer read-only secret mounts and `*_FILE`. No secret is printed on startup.
The HTTP listener and artifact/static directories are fixed by this image;
external `DATABASE_URL` and disabled authentication are refused. Updater
configuration is ignored: this image never mounts `docker.sock`.

The only persistent volume is `/data`: `pg/` stores the database, `private/`
stores bootstrap credentials and storage/release markers, and `socket/`
contains PG's local socket. PG listens only on container loopback with SCRAM
TCP authentication; it is never published. The application DB role is not a
superuser and cannot create roles/databases. Proxy and PG use separate
unprivileged users; the init/supervisor retains root with the five listed
capabilities to manage fixed paths, launch those users and signal children.
This is **not a fully rootless image**. Do not use `--privileged`, share its
PID namespace or place other workloads in this container. The Proxy UID and
root can access Proxy credentials; PG cannot read their files or process
environment. A Docker administrator still controls all of them.

### Single-container updates and backups

Update by pulling an immutable version/SHA, stopping the old container with
65 seconds of grace, and recreating it with **the same `/data` volume and URL /
secret configuration**. There is no in-place `/admin` updater. Proxy starts
after PG readiness and runs the existing idempotent schema migrations. Child
crashes back off and exit nonzero after more than five failures in five
minutes. PG failure first stops Proxy; clean shutdown stops Proxy before PG's
fast shutdown/checkpoint. Use the Docker restart policy for availability;
repeated failures still require operator diagnosis, not empty-DB recreation.

Take a cold backup before an update. It includes credentials, signing keys,
sessions and audit/message data and is **as sensitive as the live server**.
Keep it encrypted and restrict the output file to `0600`:

```bash
umask 077
docker stop -t 65 treer
docker run --rm --volumes-from treer:ro --entrypoint /bin/tar \
  ghcr.io/evoevolver/treer:vX.Y.Z -C /data -czf - . > treer-backup.tar.gz
docker start treer
```

Use your encrypted backup destination/tool; do not commit or upload the archive
unencrypted. Restore while the server is stopped, into a **new empty volume**,
using the matching image version; keep the original volume untouched:

```bash
docker volume create treer-restored
docker run --rm -i --mount type=volume,src=treer-restored,dst=/data \
  --entrypoint /bin/tar ghcr.io/evoevolver/treer:vX.Y.Z \
  -C /data -xzf - < treer-backup.tar.gz
```

Recreate the server with `src=treer-restored` and its original secret mounts /
URLs. Do not run two servers against one volume (the supervisor also takes an
exclusive volume lock). Restore only trusted backups: database/configuration
files are executable server input. External symlinks in `/data` are rejected.

PG major versions never upgrade in place: a wrong `PG_VERSION`, incomplete
initialization, unknown storage format or older release version refuses to
start. Do not delete markers to bypass these checks. For a major upgrade:

1. Make the cold backup above, restart the **old image**, and export the DB
   with `umask 077; docker exec -u postgres treer pg_dump -h /data/socket -U
   postgres -Fc treer > treer-db.dump`. Quiesce clients first. This logical
   dump alone omits the private bootstrap files; retain the cold backup too.
2. Stop the old server. Start the **new-major image** with a new volume,
   `TREER_SINGLE_MAINTENANCE=restore`, and no published port. Wait for PG
   readiness via `docker exec -u postgres NEW pg_isready -h /data/socket`.
   Proxy is intentionally disabled and HTTP health is unavailable.
3. Restore with `docker exec -i -u postgres NEW pg_restore -h /data/socket
   -U postgres --exit-on-error --no-owner --role=treer -d treer < treer-db.dump`.
   Stop the maintenance container; recreate normally on that new volume,
   without the maintenance variable. Supply the intended admin credential.
4. Validate login, workspaces, enrollment and messages before switching DNS.
   Keep the old image/volume/backup for recovery. Future PG-major releases
   must explicitly test this procedure; this image currently supports PG17.

Never start an older SHA on a database migrated by a newer SHA. Release-version
markers reject version downgrades, but **cannot infer ancestry between different
SHAs with the same version**; rollback means an old image **and its matching
backup volume**, not just changing the image tag.

Compared with Compose: one replica, no internal NATS/JetStream durable event
history or multi-Proxy coordination, no socket updater, and shared container
lifecycle for PG/Proxy. Committed Core Messages remain in PostgreSQL. Each
architecture's image bundles its native Linux machine artifacts; missing
platforms fail closed instead of downloading `latest`. For other machine
platforms provide an explicitly trusted, matching release artifact base URL.

### Build and verify the single image

From the repository root, with a clean exact-SHA checkout:

```bash
docker build -f Dockerfile.single --build-arg TREER_BUILD_COMMIT="$(git rev-parse HEAD)" \
  --build-arg TREER_VERSION=0.1.14 -t treer-single:local .
env -i PATH="$PATH" python3 -I -m unittest discover -s deploy/single-container/tests
node deploy/single-container/smoke.mjs treer-single:local
```

The smoke script uses only disposable containers, a private Docker network,
random names/ports, test credentials and a disposable enrolled machine. It
checks static/API Host separation, health, admin login/registration, native
installation/enrollment, Agent creation and persistence across server restart;
it does not touch any real machine. `runtime-smoke.py` additionally tests real
PG against a fake Proxy to exercise volume/permissions without a Rust build.
Base-image indexes are pinned by digest; release rebuilds still pick up apt
security updates. The binary checksum manifest in `/app/SHA256SUMS` and
`/app/build.json` identify the native binaries and exact source SHA; the build
verifies hashes before installing them. CI must smoke each native architecture
before publishing the combined image manifest.

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

## Explicit public service ports

HTTP service domains use the Proxy's `TREER_INGRESS_PUBLIC_URL` wildcard base.
Core assigns the individual service name; Apps do not supply custom hosts.
Workspace authentication is the default. Configure wildcard DNS/TLS at your
edge before exposing the base domain.

Anonymous raw TCP is opt-in: set `TREER_PUBLIC_TCP_PORT_RANGE=start-end` on the
Proxy and expose that same range through Compose or your deployment firewall.
`TREER_PUBLIC_TCP_BIND_ADDRESS` defaults to `0.0.0.0`. The maximum configured
range is 1,024 ports. Merely setting the range does not publish a service; an
explicit `treer service publish SERVICE_ID --tcp --access public` allocates a
mapping. The service must implement its own protocol authentication. This
repository change does not modify an existing deployment or firewall.
