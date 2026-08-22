# Release process

- Status: maintained
- Last reviewed: 2026-08-21 at `239f9c6`

Treer promotes an explicit Git commit through Canary before Production. A
release is not a branch name, a mutable `latest` label, or the contents of an
operator's current directory. The ignored `.treer/releases/` directory stores
the local release manifest and the exact frontend artifact that passed Canary.

## Platform boundary

Railway runs the Proxy replicas, PostgreSQL, NATS, and the two-machine Canary
test services. Cloudflare Workers Static Assets runs the independent React App:

| Environment | App | Proxy |
| --- | --- | --- |
| Canary | `https://app.canary.treer.ai/` | `https://proxy.canary.treer.ai/` |
| Production | `https://app.treer.ai/` | `https://proxy.treer.ai/` |

The App Worker serves Vite output directly from Cloudflare. Only `/health` and
`/config.json` invoke Worker code. Runtime configuration keeps one frontend
bundle independent of the Proxy origin.

The optional Cloudflare Workers Builds integration validates `web/` on pushes
to the configured branch and uploads an inactive Canary version. It does not
deploy that version to traffic. Canary and Production remain explicit operator
promotions, so an ordinary source push cannot bypass the release gate.

Workers Builds requires a one-time Cloudflare GitHub App authorization. Connect
`EvoEvolver/treer` to `treer-app-canary` and use these settings:

| Setting | Value |
| --- | --- |
| Production branch | `main` |
| Root directory | `/` |
| Build command | Empty |
| Deploy command | `pnpm --dir web install --frozen-lockfile && pnpm --dir web build && pnpm --dir web worker:upload:canary` |
| Non-production/version command | Same as the deploy command |
| Build watch include path | `web/**` |
| Build cache | Enabled |

The self-contained command is intentional: Workers Builds may invoke a version
or deploy command without a separate dependency-install/build phase. Its final
step uses `wrangler versions upload`, not `wrangler deploy`, so a successful
source build uploads an inactive Canary version without changing active Canary
or Production traffic. Production is only selected explicitly with
`--env production` during operator promotion.

Canary version and branch-alias Preview URLs are public internet endpoints.
They make an uploaded frontend candidate inspectable while the base
`treer-app-canary.<account>.workers.dev` route remains disabled. Preview URLs
use Canary runtime configuration and are build evidence only: they do not
deploy the Railway Proxy, Core Message code, or Mail/Telegram App processes.

## Machine artifacts

The `Build release artifacts` GitHub workflow runs only by manual dispatch or
for a pushed `v*` tag. Its matrix uses native runners for `linux-x86_64`,
`linux-aarch64`, `darwin-x86_64`, and `darwin-aarch64`. Each platform artifact
contains the CLI, Host, Controller, source commit metadata, and checksums. The
workflow has read-only repository permission and never publishes a GitHub
Release or writes to R2.

After the workflow succeeds, collect the exact commit into the ignored local
artifact tree:

```bash
just collect-artifacts HEAD
```

This command requires authenticated `gh` and `jq`. It selects a successful run
whose `headSha` is the requested commit, validates all four metadata files and
checksums, restores executable permissions, and writes `dist/<platform>/`.
Set `TREER_ARTIFACT_RUN_ID` when a manually dispatched run must be selected
explicitly. The existing `artifacts-prepare`, `artifacts-canary`, and
`artifacts-stable` commands then sign and distribute those bytes without
recompiling them.

## Workspace Apps

Mail and Telegram are source Apps, not additional Rust machine binaries and not
part of the Cloudflare control-plane artifact. Deploy them from the same clean
source commit, build Mail's `web/dist`, and record the revision. Configuration,
Python bytecode, channel secrets, and App state are never release artifacts.
App supervision, rollback, state migration, and isolation use the deployment's
normal service tooling; Treer has no App package installer.

## Canary release

The operator needs authenticated Railway and Wrangler CLIs, Docker, `just`,
`pnpm`, Python 3, PostgreSQL client tools, `jq`, and `curl`. Start from a clean
checkout of the candidate commit:

```bash
just release-canary HEAD
```

The command refuses a dirty worktree, a revision other than `HEAD`, or a commit
not contained in `origin/main`. It runs the complete local gate, stores the
built `index.html` by SHA-256, deploys the Canary Proxy and App, and runs the
black-box two-machine test. Success produces:

```text
.treer/releases/<full-commit>/manifest.json
```

The manifest records the commit, frontend checksum, Railway deployment ID,
Cloudflare Worker version ID, endpoints, and test timestamp. Failure never
creates an eligible manifest.

The deployment scripts also set `TREER_BUILD_COMMIT` on the Railway Proxy
service before each build. The Docker builder embeds that candidate commit in
the Proxy-bundled Host, Controller, and CLI artifacts; it is release metadata,
not a mutable runtime version override.

Canary and Production Proxy deployment scripts explicitly set
`TREER_ENABLE_CORE_MESSAGES=true`. It defaults off outside those scripts. App
process supervisors are configured separately after code, config, secrets,
state backup, and Policy are ready.

## Production promotion

Check out the same clean commit and retain its `.treer/releases` directory,
then run:

```bash
just promote-production .treer/releases/<full-commit>/manifest.json
```

Promotion requires `status: canary_passed`, verifies the commit and artifact
checksum, deploys the Production Proxy and the exact Canary frontend artifact,
then checks Proxy health, App health/configuration, and wildcard TLS routing.
It updates the manifest to `status: production_deployed` with Production IDs.

Only one release may run in a checkout at a time. The scripts use
`.treer/release.lock` to reject overlapping operators.
Self-hosted forks can set `TREER_RELEASE_REMOTE_REF` to their protected release
branch.

## Compatibility and rollback

- Add Proxy API fields before the App begins using them. Stop App use before a
  later release removes an API.
- Use expand-first PostgreSQL changes so old and new Proxy replicas can overlap.
- Before a legacy Mail cutover, back up its SQLite/PostgreSQL database, stop the
  Rust Mail writer, dry-run and execute `apps/mail/migrate.py` with a required
  `--actor` identity, compare and retain the checksum/checkpoint report, start
  the Mail App, and require users to log in again.
  Rollback to the old writer is safe only before any new Core Message write;
  after that point, repair forward and keep Core authoritative.
- Back up each App's state before replacing or moving it. Telegram
  needs its offset and ID-mapping SQLite state to avoid avoidable external
  duplicates. Keep Bot tokens and Mail configuration out of source and release
  artifacts, and provide them through the deployment secret mechanism.
- Telegram's Bot API does not accept a client idempotency key. A lost successful
  send response can produce a visible duplicate on retry; rollback and release
  notes must not claim external exactly-once delivery.
- To stop channel traffic, stop the App processes. Do not turn off Core Message
  routes while a bridge still has an unacknowledged external delivery. Feature
  flags do not delete Message rows or App state.
- Before the first stable release, a Controller protocol bump may deliberately
  require a coordinated Proxy rollout and machine re-enrollment. Record that
  boundary in the release notes and reset Canary as one unit. Once stable
  releases begin, keep the current and previous Controller protocols usable
  during machine rollout.
- Roll back the App with `wrangler rollback --env production <version-id>`.
- Roll back the Proxy to its prior Railway deployment. Database contraction is
  a separate release and is never part of an automatic code rollback.

The first implementation rebuilds the Proxy from the same commit in each
Railway environment. Moving to a single OCI image digest is a future hardening
step; the frontend artifact is already byte-identical across promotion.
