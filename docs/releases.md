# Release process

- Status: maintained
- Last reviewed: 2026-10-11

Treer promotes an explicit Git commit through Canary before Production. A
release is not a branch name, a mutable `latest` label, or the contents of an
operator's current directory. The ignored `.treer/releases/` directory stores
the local release manifest and the exact frontend artifact that passed Canary.

## Security release upgrade notes — 2026-10-12

- Policy Provider editing is now denied until an operator configures its
  `POLICY_ADMIN_TOKEN`; existing editing clients must supply the corresponding
  Bearer token. This token is a separate operator capability, not a Treer OAuth
  administrator role. Keep it out of source, shared browser bundles and persisted
  client storage. The Policy UI accepts it in a password field for page memory
  only; same-account workloads still are not isolated from its server environment.
- Upgrade Core before switching Agents to the strict launcher. An old Controller
  cannot provide the required forwarding provenance and the new launcher returns
  HTTP 403. Machine rollout order is **Proxy → Controller → CLI → coordinated
  Agent restart onto the new launcher**. A Canary deployment does not update
  enrolled machines or restart their Agents automatically.
- Unix bootstrap now verifies every binary against same-origin `SHA256SUMS`
  before installing any of them. Missing, invalid or mismatched checksums abort;
  this is not independent publisher authentication or rollback protection.
- Workspace Policy still defaults to allow without an enforcing Policy. The
  shared machine account is not a sandbox; token revocation windows, legacy
  identity limitations and other [unsupported security claims](security.md#unsupported-claims)
  remain. Do not present this release as a security certification.

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
`linux-aarch64`, `darwin-x86_64`, `darwin-aarch64`, and `windows-x86_64`. Windows
uses a native MSVC runner and gates artifacts on IPC/ConPTY, Host lifecycle,
Controller/CLI, installer checksum, and PATH tests. Each platform artifact
contains the CLI, Host, Controller, source commit metadata, and checksums. The
workflow has read-only repository permission and never publishes a GitHub
Release or writes to R2.

Windows binaries are PE `.exe` files in the build output. Staged artifacts and
public download keys retain the three logical binary names without a suffix;
the PowerShell installer saves them with `.exe`. Each platform directory also
contains `SHA256SUMS`, available through `/artifacts/<platform>/SHA256SUMS` on a
Proxy that carries the candidate artifact tree. Existing fallback release
servers must supply `SHA256SUMS-<platform>` as well as the three binaries;
Windows installation fails closed if that checksum asset is absent.

Installed Controller hot updates consume the separately signed R2 channel and
version manifest rather than these unsigned flat Proxy endpoints. Provision
the publisher's SPKI Ed25519 public key locally before updating. Both signatures,
the channel's manifest SHA256, and both binaries' signed sizes/SHA256 must pass
before any downloaded code runs. See [signed artifact distribution](operator-guide.md#publish-signed-r2-releases)
for publishing and [machine updates](operator-guide.md#controller-hot-updates)
for invocation. Local/unsigned candidate artifacts remain available to initial
installers and Canary tests; they cannot bypass the updater's signature checks.

After the workflow succeeds, collect the exact commit into the ignored local
artifact tree:

```bash
just collect-artifacts HEAD
```

This command requires authenticated `gh` and `jq`. It selects a successful run
whose `headSha` is the requested commit, validates all five metadata files and
checksums, restores executable permissions, and writes `dist/<platform>/`.
Set `TREER_ARTIFACT_RUN_ID` when a manually dispatched run must be selected
explicitly. The existing `artifacts-prepare`, `artifacts-canary`, and
`artifacts-stable` commands then sign and distribute those bytes without
recompiling them.

Hosted deployment automatically runs `scripts/ensure-machine-artifacts.sh`.
It reuses verified `dist/` bytes or dispatches the native workflow with the exact
source commit and waits for all five platforms. Manual dispatch can build a
specific commit with its `revision` input; its workflow `headSha` may differ from
the built revision, so the source metadata is authoritative. Stale or incomplete
local `dist/` trees fail validation rather than being overwritten. To prepare
the artifacts independently, run `just ensure-artifacts HEAD`.

`Dockerfile.hosted` bundles all twelve native binaries without recompiling them.
The upload directory contains only `git archive` source from the clean candidate
commit and verified artifacts. Its generated Railway configuration selects this
Dockerfile; self-hosted/GHCR builds continue using `Dockerfile`. Validation runs
both before changing deployment variables and inside the hosted image build.
After deployment, all twelve public downloads must return HTTP 200 without a
redirect and match the collected bytes. This prevents macOS installs from
silently falling back to an older GitHub `latest` Release.

## Workspace Apps

Mail and Telegram are source Apps, not additional Rust machine binaries and not
part of the Cloudflare control-plane artifact. Deploy them from the same clean
source commit, build Mail's `web/dist`, and record the revision. Configuration,
Python bytecode, channel secrets, and App state are never release artifacts.
App supervision, rollback, state migration, and isolation use the deployment's
normal service tooling; Treer has no App package installer.

## Canary release

For routine releases, use the wrapper from the development checkout:

```bash
just canary HEAD --dry-run
just canary HEAD --push
# Retry the exact commit after fixing a failed build/deployment:
just canary <full-commit> --resume
# Optional completion message; requires the actual managed caller to match:
just canary HEAD --push --notify leader --agent deployer
```

The wrapper resolves the revision once and creates a detached worktree under
`.treer/canary-checkouts/<commit>/`, leaving concurrent development untouched.
`--push` only fast-forwards `origin/main`; divergent history requires a reviewed
merge. It installs frontend dependencies and runs the maintained release pipeline
below, including all five native artifact builds. It preserves the worktree and
release manifest for inspection and retry. `--dry-run` only prints the local
plan; it performs no writes or network calls. The wrapper never updates local
CLI binaries, restarts a local Host/Controller, or promotes Production.

`--resume` reuses the completed local gate only when its receipt matches the
exact commit and unchanged frontend SHA-256. Missing or mismatched receipts
rerun the full gate; deployment, artifact verification, and two-machine tests
always run again. These receipts are local operator state, not signed build
attestations. Native workflow failures remain failures: inspect the failed job
and explicitly rerun it with `gh run rerun <run-id> --failed` before resuming.
No failing test is skipped or automatically retried by the wrapper.
The deployed frontend must also match the candidate HTML byte for byte.

`--notify` sends only after the release passes, with an idempotency key derived
from the full commit. `--agent` checks the caller before any release changes and
again before sending. A mismatched caller fails instead of borrowing another
Agent's credentials. Canary login cookies stay in shell memory; the test script
does not create credential files. `just canary-scripts-test` checks the wrapper,
checkpoint invalidation, and in-memory cookie handling without a deployment.

The operator needs authenticated GitHub, Railway and Wrangler CLIs, Docker, `just`,
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
service before each build. The hosted builder embeds the candidate commit in
the Proxy; the native workflow embeds the same commit in the bundled Host,
Controller, and CLI artifacts. It is release metadata, not a mutable runtime
version override. Deployment updates the downloadable binaries; already
installed machines require an explicit Controller update/restart.

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
checksum, verifies all five machine artifact sets, deploys the Production Proxy
and the exact Canary frontend artifact,
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
- Keep the legacy Controller protocol field on a version understood by the
  previous Proxy while advertising additive `supported_protocols` and
  capabilities. Roll out Proxy support before selecting a new protocol version.
  Capability-gated commands must fail as `unsupported_command` on older
  Controllers instead of disconnecting the machine. A deliberate incompatible
  protocol reset still requires a coordinated Canary rollout and an explicit
  release note.
- Roll back the App with `wrangler rollback --env production <version-id>`.
- Roll back the Proxy to its prior Railway deployment. Database contraction is
  a separate release and is never part of an automatic code rollback.

## GHCR images for self-host

After Canary passes, tag the same commit `vX.Y.Z` matching `Cargo.toml` and
push the tag. The `Publish GHCR images` workflow builds native `linux/amd64`
and `linux/arm64` images for Proxy, App, and the updater sidecar, then writes
immutable `vX.Y.Z` and `sha-<commit>` tags plus a moving `canary` pointer.

Do not publish `latest`. After Production promotion, dispatch the same
workflow with `channel=stable` and the version tag. That retags existing
digests; it does not rebuild.

Self-hosted Compose pulls those images (`image:`, not `build: .`) and applies
later versions from `/admin`. See [Self-hosted Compose](../deploy/README.md).
Hosted Railway continues to rebuild the Proxy from the promoted commit in each
environment. The frontend artifact is already byte-identical across promotion.
