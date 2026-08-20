# Canary environment

- Status: maintained
- Last reviewed: 2026-08-19

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

## Deploy and test

The operator needs authenticated Railway and Wrangler CLIs plus `curl`, `jq`,
`pnpm`, Docker, and `just`:

```bash
just release-canary HEAD
```

`release-canary` requires a clean committed revision, runs `just check`, deploys
the current worktree to the Railway Proxy and the built App artifact to
Cloudflare, and verifies their health endpoints. It also sets the Canary public
URLs explicitly so this environment cannot accidentally emit Production
installation or App links.

`test-canary` performs a black-box workflow against the deployed Proxy:

1. Log in with Canary's Railway-managed admin password and reuse the dedicated
   `canary-tester@treer.invalid` account and `canary-e2e` workspace.
2. Mint two one-use enrollment keys and refresh two dedicated, long-lived
   Railway machine services. Their stable service IDs avoid Railway's daily
   service-creation quota while each run still rotates machine credentials.
3. Each machine downloads the deployed Host, Controller, and CLI artifacts,
   enrolls through the public Proxy URL, and serves a machine-specific HTTP
   response on its loopback interface.
4. Register the second machine's HTTP server as a machine service and virtual
   host, then run `curl` as a command Agent on the first machine.
5. Publish the same service through wildcard HTTPS and fetch the returned URL
   directly from the tester.
6. Confirm the A-to-B payload appears in directional traffic accounting.
7. Delete only the agents, machine service, virtual host, and ingress created by
   the run. The dedicated Railway services and Treer machine identities remain
   available for the next release.

The default fixtures are the two maintained Canary machine services. Override
`TREER_CANARY_MACHINE_A_SERVICE`, `TREER_CANARY_MACHINE_B_SERVICE`,
`TREER_CANARY_MACHINE_A_NAME`, and `TREER_CANARY_MACHINE_B_NAME` together when
moving the test fleet. The test intentionally fails when wildcard DNS or
certificate issuance is not ready. Set `TREER_CANARY_TEST_TIMEOUT` for slow
Railway builds. Set `TREER_CANARY_DEPLOY_TIMEOUT` separately for control-plane
deployment waits.
For failure investigation, `TREER_CANARY_KEEP_RESOURCES=1 just test-canary`
leaves the run's logical Treer resources in place. The two Railway fixtures are
never removed by this test.
`TREER_CANARY_SKIP_PUBLIC=1` is available only to isolate internal networking
and accounting failures while DNS is unavailable. A skipped public-ingress test
is never eligible for Production promotion.

## Promotion rule

Production promotion is a separate operator action. A successful Canary run
writes an ignored release manifest and preserves the exact App artifact. Run
`just promote-production <manifest>` from the same clean commit. The promotion
script rejects other commits, modified artifacts, failed tests, and overlapping
release processes. See [Release process](releases.md) for the complete contract.
