# Canary environment

- Status: maintained
- Last reviewed: 2026-08-19

Canary is the required deployment target before production. It uses a separate
Railway environment, PostgreSQL database, NATS instance, Proxy, App, and
wildcard ingress domain. A successful build or health endpoint is not enough to
promote a revision: the disposable two-machine test must also pass.

## Railway resources

The default scripts target the `Treer` project in the `canary` environment:

| Resource | Default |
| --- | --- |
| Proxy | `https://treer-proxy-canary.up.railway.app/` |
| App | `https://treer-app-canary.up.railway.app/` |
| Service ingress | `https://*.canary.apps.treer.ai/` |

IDs are defaults rather than hidden discovery. Override
`TREER_RAILWAY_PROJECT_ID`, `TREER_CANARY_ENVIRONMENT`,
`TREER_CANARY_PROXY_SERVICE`, or `TREER_CANARY_APP_SERVICE` when running the
same workflow in another Railway project.

The wildcard domain requires these DNS-only records. Railway shows the current
target values through `railway domain status`; do not assume old target values
remain valid.

```text
CNAME *.canary.apps             <Railway traffic target>
CNAME _acme-challenge.canary.apps <Railway authorization target>
TXT   _railway-verify.canary.apps <Railway verification token, when unverified>
```

`just deploy-canary` prints the current required targets and certificate state.
The verification TXT record is conditional; remove or retain it according to
the DNS provider's normal ownership-verification policy after Railway reports
the domain verified.

## Deploy and test

The operator needs an authenticated Railway CLI plus `curl` and `jq`:

```bash
just deploy-canary
just test-canary
```

`deploy-canary` uploads the current worktree to the Canary Proxy and App,
waits for both deployments, and verifies their health endpoints. It also sets
the Canary public URLs explicitly so this environment cannot accidentally
emit Production installation or App links.

`test-canary` performs a black-box workflow against the deployed Proxy:

1. Log in with Canary's Railway-managed admin password and reuse the dedicated
   `canary-tester@treer.invalid` account and `canary-e2e` workspace.
2. Mint two one-use enrollment keys and create two disposable Railway services.
3. Each service downloads the deployed Host, Controller, and CLI artifacts,
   enrolls through the public Proxy URL, and serves a machine-specific HTTP
   response on its loopback interface.
4. Register the second machine's HTTP server as a machine service and virtual
   host, then run `curl` as a command Agent on the first machine.
5. Publish the same service through wildcard HTTPS and fetch the returned URL
   directly from the tester.
6. Confirm the A-to-B payload appears in directional traffic accounting.
7. Delete the Treer machine records and Railway services by exact ID.

The test intentionally fails when wildcard DNS or certificate issuance is not
ready. Set `TREER_CANARY_TEST_TIMEOUT` for slow Railway builds. Set
`TREER_CANARY_DEPLOY_TIMEOUT` separately for control-plane deployment waits.
For failure investigation, `TREER_CANARY_KEEP_RESOURCES=1 just test-canary`
leaves the two temporary Railway services in place and prints their exact IDs;
delete them after inspecting their logs.
`TREER_CANARY_SKIP_PUBLIC=1` is available only to isolate internal networking
and accounting failures while DNS is unavailable. A skipped public-ingress test
is never eligible for Production promotion.

## Promotion rule

Production promotion is a separate operator action. Record the exact Git
revision deployed to Canary, require `just check` and `just test-canary` to pass
for that revision, then deploy that same committed revision to Production. Do
not rebuild from a dirty or different worktree between Canary and Production.
