# Self-hosted Treer

Self-hosted Treer runs PostgreSQL, NATS, Proxy, the browser App, and an updater
sidecar from published GHCR images. This is the operator path for a machine you
control. The hosted Railway plus Cloudflare path is unchanged and does not
start this sidecar.

## Single container

Build the single-container **central server** locally from the checkout.
There is no registry publishing for this image; the script never pushes.
It contains PostgreSQL 17, the standalone Proxy, the browser Console, and native
Linux machine binaries built from the same commit. Machines, Controllers,
Hosts, Agent runtimes and workspace Apps run separately and enroll normally;
they are not started inside this container. The Compose installation below is
still supported.

From a clean checkout, with Git and Docker (including Buildx) installed:

```bash
./scripts/build-single-image.sh
```

The script compiles the committed source in Docker, packages the Docker engine's native Linux machine
architecture and server image, and loads it
as `treer-single:<12-character commit>`. It prints the startup command and URLs
without starting a server. The first build needs network access to fetch pinned
base images and locked Rust/npm dependencies; compiling the server and machine binaries takes
time and uses Docker build cache for subsequent builds. Rust, Node and PostgreSQL
do not need to be installed on the host. Uncommitted tracked changes are refused;
ignored/untracked files and `.git` never enter the build context.

Use the tag printed by the script (replace `COMMIT12` below), then start:

```bash
docker run -d --name treer --restart unless-stopped --stop-timeout 65 \
  --read-only --tmpfs /tmp:rw,nosuid,nodev \
  --cap-drop ALL --cap-add CHOWN --cap-add DAC_OVERRIDE \
  --cap-add SETUID --cap-add SETGID --cap-add KILL \
  --security-opt no-new-privileges:true \
  -p 127.0.0.1:8080:8080 --mount type=volume,src=treer-data,dst=/data \
  treer-single:COMMIT12
```

Docker creates the named volume if absent. Open the Console at
`http://console.treer.localhost:8080`; its API is `http://proxy.treer.localhost:8080`. These are two
different browser origins on **one port**, and share a site for the existing
`SameSite=Strict` sessions. Chromium and Firefox resolve these loopback names
without DNS setup. Preserve that
separation: the Proxy serves Console files only for the configured Console
Host, API/embedded UIs only for the Proxy Host, and rejects other Hosts.
The local addresses work only on the server machine, not remote clients.

When `ADMIN_PASSWORD` / `ADMIN_PASSWORD_FILE` is absent, a random platform
administrator password is generated once. Retrieve it deliberately in a
private terminal (this command displays the secret):

```bash
docker exec treer cat /data/private/admin-password
```

Use it at `http://console.treer.localhost:8080/admin` to create an invitation, then register
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

Both public names must share the same scheme and registrable domain (site),
while remaining different origins. `localhost` plus `127.0.0.1` is **not** a
working local pair: Strict sessions are withheld across those sites.
The reverse proxy must preserve the original `Host` and support WebSockets;
its certificate must cover both names. Do not expose plain HTTP publicly.
Cookie/Origin/CSRF checks remain enabled. Identical origins cannot safely embed
untrusted App/Agent UIs: the Core single-origin mode warns and disables framing;
use two names for the full Console. Wildcard service ingress additionally needs
its own DNS/TLS domain via `TREER_INGRESS_PUBLIC_URL`. Prefer a different
registrable domain for untrusted App content, e.g. `apps.example.net`; if it
shares the Console site, SameSite is not an additional barrier and the exact
Origin/CookieOriginClass checks are essential. Host routing is not authorization.
Cookies are not isolated by port: do not run untrusted services on another
port of these same hostnames. Do not route arbitrary
unknown Hosts to the Console. Configure routable public URLs before enrolling
remote machines; loopback defaults will not connect from another machine.

Configuration is through environment variables (no manual config editing):

| Variable | Default / behavior |
| --- | --- |
| `ADMIN_PASSWORD`, `ADMIN_PASSWORD_FILE` | Env takes precedence; otherwise private file, otherwise generated on first start |
| `POSTGRES_PASSWORD`, `POSTGRES_PASSWORD_FILE` | Random on first start; persisted, mismatched replacement rejected (not a DB rotation API) |
| `TREER_PROXY_PUBLIC_URL` | `http://proxy.treer.localhost:8080/` |
| `TREER_APP_PUBLIC_URL` | `http://console.treer.localhost:8080/` |
| `TREER_ENABLE_CORE_MESSAGES` | `true`, works without NATS |
| `TREER_NATS_URL` | Unset; external NATS is opt-in, none is installed |
| `TREER_SINGLE_MAINTENANCE` | Unset; `restore` starts only PG for an offline logical restore |
| Other Proxy OAuth, invitation, audit and ingress variables | Same as the normal Proxy |

Explicit empty secrets, unsafe files and partial initialization fail closed.
`*_FILE` files must be regular, single-link, root-owned, private (`0600`), and
contain one nonempty UTF-8 line. Secrets supplied directly as environment
variables are visible to Docker administrators through `docker inspect`;
prefer read-only secret mounts and `*_FILE`. No secret is printed on startup.
Docker/Compose secrets with their default `0444` permissions are deliberately
rejected. On a Linux Docker host, create a root-owned private bind file:

```bash
python3 -c 'import secrets; print(secrets.token_urlsafe(48))' | \
  sudo install -o root -g root -m 0600 /dev/stdin ./treer-admin.secret
```

Add `--mount type=bind,src="$PWD/treer-admin.secret",dst=/run/secrets/treer-admin,readonly`
and `-e ADMIN_PASSWORD_FILE=/run/secrets/treer-admin` to the run command. The
input file is a secret, not a config example: never commit it or send its bytes
in an issue, log or shell argument. Its read-only mount does not change its
required owner/mode.
The HTTP listener and artifact/static directories are fixed by this image;
external `DATABASE_URL` and disabled authentication are refused. Updater
configuration is ignored: this image never mounts `docker.sock`.

The image does not encrypt its volume; use encrypted host storage where needed.
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
Do not put unrelated files in `/data`: startup scans the volume for external
symlinks, which can take longer on large databases. Debian setuid/setgid bits
are removed in the image; `/app/system-packages.txt` records the installed
package versions (the apt security repository itself is not snapshotted).

### Local browser compatibility

| Engine tested with isolated profiles | `*.treer.localhost` | `.test` mapping below |
| --- | --- | --- |
| Chromium (Playwright) | Resolution and Strict-cookie cross-origin requests passed | Not separately tested |
| Firefox 153 (Playwright) | Resolution and Strict-cookie cross-origin requests passed | Not separately tested |
| WebKit 26.5 (Playwright) | Resolves, but cross-origin Strict cookie is withheld | Passed with local DNS-proxy mapping |
| Actual Safari.app | Not tested | Not tested; do not infer Safari support from WebKit |

For Safari/WebKit compatibility, map ordinary same-site local names instead.
Add this line to your own machine's hosts file (the installer does not edit it):

```text
127.0.0.1 console.treer.test proxy.treer.test
```

Recreate the container with `TREER_APP_PUBLIC_URL=http://console.treer.test:8080/`
and `TREER_PROXY_PUBLIC_URL=http://proxy.treer.test:8080/`, then open
`http://console.treer.test:8080`. Both URLs must change together. The `.test`
names are local only, not public deployment names; login/CSRF now use these
new exact origins. WebKit was tested using a local HTTP proxy as DNS mapping,
without modifying this machine's hosts file. Real Safari remains an operator
verification step. Do not relax cookies, CORS or Origin checks to work around
browser differences.

### Single-container updates and backups

Update by checking out a clean new commit and rerunning
`./scripts/build-single-image.sh`. Back up first, then stop the old container
with 65 seconds of grace and recreate it using the printed local image tag with **the same `/data` volume and URL /
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
IMAGE=$(docker inspect --format '{{.Config.Image}}' treer)
docker stop -t 65 treer
docker run --rm --volumes-from treer:ro --entrypoint /bin/tar \
  "$IMAGE" -C /data -czf - . > treer-backup.tar.gz
docker start treer
```

Use your encrypted backup destination/tool; do not commit or upload the archive
unencrypted. Restore while the server is stopped, into a **new empty volume**,
using the saved matching local image tag (replace `COMMIT12` below); keep the
original volume untouched:

```bash
docker volume create treer-restored
docker run --rm -i --mount type=volume,src=treer-restored,dst=/data \
  --entrypoint /bin/tar treer-single:COMMIT12 \
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
lifecycle for PG/Proxy. Committed Core Messages remain in PostgreSQL. The local image
bundles only the Docker engine’s Linux machine architecture; CI images bundle
both Linux architectures. Missing machine platforms fail closed instead of downloading `latest`. For other machine
platforms provide an explicitly trusted, matching release artifact base URL.

### Build and verify the single image

For a local build, run `./scripts/build-single-image.sh` from a clean checkout.
It archives committed files into a private temporary directory (excluding
untracked/ignored files and `.git`), builds only the Docker engine's native
amd64/arm64 architecture, and loads `treer-single:<12-character commit>` locally.
It never starts containers or publishes images. Docker receives a whitelist
environment, without Agent/cloud/Git credentials; only a local Unix-socket
Docker engine is supported. No registry credentials or private dependency
registries are supported by this build path.

Local binaries are compiled with locked dependencies inside pinned base images.
`/app/build.json` and the `io.treer.provenance` label say `local-build`:
checksums establish consistency, **not release provenance**. A local build is
not equivalent to a CI release artifact, is not guaranteed reproducible, and
must not be pushed to a registry. Apt/npm/Cargo downloads require network
access; the first build can take substantial time and disk space. No host
Rust, Node or PostgreSQL installation is needed.

Run `just single-image-smoke` to build through the script and check real HTTP
health in a disposable container/volume with a random loopback port. This heavy test is deliberately not part of `just check`.

The separate CI prebuilt-input path remains unchanged. With a clean exact-SHA
checkout, stage the trusted
native release artifacts under `.single-container-input/` before building:

```text
.single-container-input/
  machines/linux-x86_64/{treer,treer-agent-host,treer-agent-server,SHA256SUMS,build-metadata.json}
  machines/linux-aarch64/{treer,treer-agent-host,treer-agent-server,SHA256SUMS,build-metadata.json}
  proxy/linux-aarch64/{treer-proxy,SHA256SUMS,build-metadata.json}
```

Use `proxy/linux-x86_64` for an amd64 image; the input may contain both Proxy
platforms. Metadata uses the native release schema: `schema_version:1`,
`git_commit` (40 lowercase hex), `version` (Cargo release version), `platform`.
Checksum rows are lowercase SHA256, two spaces, then the exact binary name.
Inputs must come from the trusted native release job for **the same SHA**,
including the Proxy; metadata/checksums alone do not authenticate their producer.
The Dockerfile verifies both machine platforms and the native Proxy's metadata,
checksums, regular-file types and ELF architecture before copying any binaries.
Verified public artifact directories/binaries use mode 0755 and checksum/build
metadata files use 0644, so private extraction umasks do not prevent the non-root
Proxy from serving machine downloads. This does not change `/data` secret modes.
The CI final-image path does not compile Rust or fetch release binaries. GitHub artifact downloads
can lose executable bits; verified output gets executable bits restored.
The staging directory is ignored by Git. CI downloads exact-SHA artifacts,
never a moving latest or a previous release's binaries.
The trusted native job also supplies `TREER_NATIVE_INPUT_SHA256`: SHA256 of
UTF-8 rows `sha256  relative/path\n`, sorted by relative path, covering every
input file except `.gitignore`. Pass this upstream digest separately; do not
treat a digest recomputed from untrusted downloaded bytes as provenance. The
build rejects a missing/mismatched digest and records both it and every input
file's hash in `/app/build.json`. The version must also equal `Cargo.toml`.

```bash
docker build -f Dockerfile.single --build-arg TREER_BUILD_COMMIT="$(git rev-parse HEAD)" \
  --build-arg TREER_NATIVE_INPUT_SHA256="$TRUSTED_NATIVE_INPUT_SHA256" \
  --build-arg TREER_VERSION=0.1.14 -t treer-single:local .
env -i PATH="$PATH" python3 -I -m unittest discover -s deploy/single-container/tests
node --test deploy/single-container/browser-cookie.test.mjs
node deploy/single-container/smoke.mjs treer-single:local
```

The focused browser test uses only a loopback server and synthetic cookies in
private profiles. Set `SINGLE_CONTAINER_BROWSER=firefox|webkit` to select an
installed Playwright engine; `SINGLE_CONTAINER_DNS_PROXY=1` tests the `.test`
compatibility mapping without editing hosts or reaching external networks.
The smoke script uses only disposable containers, a private Docker network,
random names/ports, test credentials and a disposable enrolled machine. It
checks static/API Host separation, duplicate-Host refusal, degraded single-origin
embedding refusal, distinct fresh signing keys/passwords, health, admin login/
registration, native installation/enrollment, Agent creation and key/data
persistence across server restart;
it does not touch any real machine. `runtime-smoke.py` additionally tests real
PG against a fake Proxy to exercise volume/permissions without a Rust build.
Base-image indexes are pinned by digest; release rebuilds still pick up apt
security updates. The binary checksum manifest in `/app/SHA256SUMS` and
`/app/build.json` identify the native binaries and exact source SHA; the build
verifies hashes before installing them. CI smokes each native architecture without publishing a single-container
image. No registry publication is enabled for this image.

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
