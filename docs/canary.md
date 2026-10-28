# Canary environment

- Status: maintained
- Last reviewed: 2026-10-11

Canary is the required deployment target before production. It uses a separate
Railway environment, PostgreSQL database, NATS instance, and Proxy plus a
separate Cloudflare App Worker and wildcard ingress domain. A successful build
or health endpoint is not enough to promote a revision: the two-machine test
must also pass.

## Railway resources

The default scripts target the `Treer` project in the `canary` environment:

| Resource | Default |
| --- | --- |
| Proxy | `https://proxy.canary.treer.ai/` |
| App | `https://app.canary.treer.ai/` |
| Service ingress | `https://*.canary.apps.treer.ai/` |

IDs are defaults rather than hidden discovery. Override
`TREER_RAILWAY_PROJECT_ID`, `TREER_CANARY_ENVIRONMENT`, or
`TREER_CANARY_PROXY_SERVICE` when running the same workflow in another Railway
project. Override `TREER_CANARY_WORKER_ENVIRONMENT` for a different Wrangler
environment.

The wildcard domain requires these DNS-only records. Railway shows the current
target values through `railway domain status`; do not assume old target values
remain valid.

```text
CNAME *.canary.apps             <Railway traffic target>
CNAME _acme-challenge.canary.apps <Railway authorization target>
TXT   _railway-verify.canary.apps <Railway verification token, when unverified>
```

The release command verifies the current required targets and certificate state.
The verification TXT record is conditional; remove or retain it according to
the DNS provider's normal ownership-verification policy after Railway reports
the domain verified.

## Frontend previews

Cloudflare version and branch-alias Preview URLs are enabled for uploaded
`treer-app-canary` versions. The base
`treer-app-canary.<account>.workers.dev` route remains disabled, so an upload
does not replace `https://app.canary.treer.ai/` or receive its traffic. Preview
URLs are nevertheless public internet endpoints; a version ID or branch alias
is an address, not authentication.

Each preview runs the frontend Worker with Canary configuration, including
`TREER_PROXY_PUBLIC_URL=https://proxy.canary.treer.ai/`. It can validate the
built application, `/health`, and `/config.json`, but it does not deploy a
candidate Railway Proxy, enable candidate Rust Core Message behavior, or start
Mail and Telegram Apps. Treat it as frontend build evidence, not as
end-to-end evidence for the PR.

## Deploy and test

The operator needs authenticated GitHub, Railway and Wrangler CLIs plus `curl`, `jq`,
`pnpm`, Python 3, PostgreSQL client tools, Docker, and `just`:

```bash
just deploy canary
just test-canary
```

Cookie-authenticated writes (`scripts/test-canary.sh` login, admin invitation and
API calls) send the exact configured
Console origin as `Origin`, taken from `TREER_CANARY_APP_URL` (default
`https://app.canary.treer.ai/`), never guessed from the Proxy URL. The Proxy
rejects such writes without it.

`just deploy canary` requires a clean checkout, deploys the checked-out commit
to the Railway Proxy and the freshly built App to Cloudflare, and verifies their
health endpoints. It also sets the Canary public URLs explicitly so this
environment cannot accidentally emit Production installation or App links. The
Proxy deployment explicitly enables `TREER_ENABLE_CORE_MESSAGES=true`;
self-hosted launches default this gate off.

Before changing Canary resources, deployment collects CLI, Host, and Controller
artifacts for Linux and macOS architectures and Windows x64 from the native GitHub
workflow at the exact candidate commit. All five sets must pass metadata and
checksum validation. The hosted Proxy image bundles them; deployment checks all fifteen
public downloads against those bytes and rejects redirects to older Releases.
See [Machine artifacts](releases.md#machine-artifacts) for preparation and retry
details. Installed Controllers still require a separate update/restart.

Run `just check` (or rely on CI for the commit) before deploying; the deploy
script itself only builds and verifies what it ships.
The Railway black-box workflow below does not currently install a Mail bridge,
contact Telegram, or automate a Mail browser. Do not cite it as external-channel
canary evidence; those are separate future fixtures.

`test-canary` performs a black-box workflow against the deployed Proxy:

1. Log in with Canary's Railway-managed admin password and reuse the dedicated
   `canary-tester@treer.invalid` account and `canary-e2e` workspace.
2. Restart the existing deployment for two dedicated, long-lived Railway
   machine services. This replaces each running instance without rebuilding or
   replacing its image and avoids Railway's daily service-creation quota.
   Railway's API occasionally rejects a restart with "Problem processing request";
   only this idempotent restart call is retried (default three attempts with a
   5-second step backoff, `TREER_CANARY_RESTART_ATTEMPTS` and
   `TREER_CANARY_RESTART_BACKOFF`), each failed attempt prints one sanitized
   summary line, and the reconnect wait and every assertion run exactly once.
3. Each machine downloads the deployed Host, Controller, and CLI artifacts,
   reuses its volume-backed machine identity, and serves a machine-specific HTTP
   response on its loopback interface.
4. Register the second machine's HTTP server as a machine service, then run
   `treer service forward` and `curl` as a command Agent on the first machine.
5. Publish the same service through wildcard HTTPS and fetch the returned URL
   directly from the tester.
6. Confirm the A-to-B payload appears in directional traffic accounting.
7. Delete only the agents, machine service, and platform-assigned ingress created by
   the run. The dedicated Railway services and Treer machine identities remain
   available for the next release.

The maintained machine image is also a development environment based on the
Microsoft Rust dev container. It includes Rust, Node.js, pnpm, Codex, GitHub
CLI, just, tmux, and the native dependencies needed to build Treer. Each service
mounts a persistent volume at `/workspace`; the Treer checkout, machine identity,
coding-agent state, and additional service data survive redeploys. Add complex
fixture services to `canary/machine` and start them from its entrypoint so tests
remain reproducible.

The machine image uses plain networking. Its explicit forward test needs no
transparent-network helper, DNS configuration, or user-selected hostname.

First-party Apps are source under `apps/`, not extra Rust machine binaries. A
dedicated deployment must use the exact source revision, provide
operator-owned configuration and secrets, and preserve App state. The two
standard Canary machines do not start those processes during the current
network-focused workflow.

Normal tests deliberately reuse the image already deployed to each service. To
publish a changed machine image while reusing its persistent identity, run:

```bash
just test-canary-provision
```

This explicit operation deploys the checked-out `canary/machine` image to both
fixed services and then runs the full E2E test. It does not erase `/workspace`
or rotate machine credentials.

If an identity is intentionally removed or its machine credential is revoked,
delete the affected volume-backed `.treer-canary/identity.json` and run
`just test-canary-enroll`. That recovery variant additionally mints and installs
new one-use enrollment keys. Do not use it for routine image changes.

The default fixtures are the two maintained Canary machine services:
`treer-canary-machine-a` and `treer-canary-machine-b`, enrolled as `canary-a`
and `canary-b`. Both mount persistent volumes at `/workspace`. Service selectors
use these explicit names so replacing an environment fixture does not leave a
retired Railway UUID in the test script. Fresh fixtures need
`just test-canary-enroll`; routine releases reuse their saved identities.
Override
`TREER_CANARY_MACHINE_A_SERVICE`, `TREER_CANARY_MACHINE_B_SERVICE`,
`TREER_CANARY_MACHINE_A_NAME`, and `TREER_CANARY_MACHINE_B_NAME` together when
moving the test fleet. The test intentionally fails when wildcard DNS or
certificate issuance is not ready. Set `TREER_CANARY_TEST_TIMEOUT` for slow
Railway builds. Set `TREER_DEPLOY_TIMEOUT` separately for control-plane
deployment waits.
For failure investigation, `TREER_CANARY_KEEP_RESOURCES=1 just test-canary`
leaves the run's logical Treer resources in place. The two Railway fixtures and
their volumes are never removed by this test.
`TREER_CANARY_SKIP_PUBLIC=1` is available only to isolate internal networking
and accounting failures while DNS is unavailable. A skipped public-ingress test
must not be taken as Canary evidence.

## Promotion rule

Production is a separate operator action: after Canary passes, run
`just deploy production` from the same clean commit. It refuses commits that are
not contained in `origin/main`. See [Release process](releases.md).
