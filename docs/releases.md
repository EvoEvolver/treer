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

Official native builds use Node 22 and lockfile-pinned `npm ci --ignore-scripts` in the trusted
CI checkout to prepare the launcher's portable runtime and UI. Dependency
lifecycle scripts are disabled; target installation stays offline and
does not require npm or Git. The bundle manifest records the exact source commit
and every file's size/SHA256. Preparation requires all six launcher entry assets,
rejects trailing dots/spaces and case-insensitive path collisions, verifies bytes, and
sets `TREER_LAUNCHER_PAYLOAD_DIR` plus `TREER_REQUIRE_LAUNCHER_BUNDLE=1` before
Cargo. Missing or invalid portable runtime payloads fail official builds; Core
also validates the actual embedded bytes. Controller signatures cover the
payload, not the externally selected Node executable. Native Node, PowerShell
and installer checks are only claimed after their platform jobs actually pass.
Local artifact staging and self-host image builds use the same preparation.


The `Build release artifacts` GitHub workflow runs only by manual dispatch or
for a pushed `v*` tag. Its matrix uses native runners for `linux-x86_64`,
`linux-aarch64`, `darwin-x86_64`, `darwin-aarch64`, and `windows-x86_64`. Windows
uses a native MSVC runner and gates artifacts on IPC/ConPTY, Host lifecycle,
Controller/CLI, installer checksum, and PATH tests. Each platform artifact
contains the CLI, Host, Controller, source commit metadata, and checksums. The
workflow has read-only repository permission and never publishes a GitHub
Release or writes to R2.

The native Windows artifact gate explicitly runs Controller
`machine_file_transfer::tests` and CLI `machine_file::tests`, in addition to the
platform filesystem tests. Each command checks its exit status before staging
artifacts. These checks require the file-transfer implementation in the fixed
candidate; changing the workflow alone is not native Windows acceptance evidence.

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

### Reusing native artifacts for web, apps and docs changes

The CLI, Host and Controller are built from `crates/`, the Cargo and toolchain
files, `skills/` and `launchers/agent`. When a candidate changes only paths under
`web/`, `apps/` or `docs/` since an earlier `canary_passed` commit, waiting for a
new five-platform build adds nothing. `scripts/ensure-machine-artifacts.sh` then
calls `scripts/reuse-machine-artifacts.sh` before dispatching a native run, but
only when no native run exists for the exact commit. It reuses the `dist/` of the
newest `canary_passed` commit in `.treer/canary-checkouts/` that is an ancestor of
the candidate, and only if all of the following hold:

- `git diff --no-renames --name-only` between the two commits lists only
  `web/`, `apps/` and `docs/` paths (a rename out of a native path is a change;
  any other or unknown path forces the normal build);
- that commit's manifest is `canary_passed`, records the SHA-256 of all 25
  artifact files, and its `dist/` still matches those recorded hashes byte for
  byte (a manifest without hashes, or a `dist/` whose binaries and checksum file
  were both rewritten, is never reused);
- its `dist/` also passes the same metadata, checksum, executable-bit and version
  verification as a fresh build;
- the copy is verified again for the candidate before use.

The reused `dist/` carries `artifact-provenance.json` (`source_commit` = the
commit the binaries were really built from, `passed_release_commit`,
`candidate_commit`, and the SHA-256 of all 25 files: three binaries,
`SHA256SUMS` and `build-metadata.json` for each platform). When the passed
release itself reused earlier binaries, the original build commit stays the
source and the ancestry and path checks run against it. `verify-machine-artifacts.sh` accepts artifacts built for
another commit only with that file and re-derives ancestry and the path
allowlist from git itself, so a forged provenance file cannot launder a native
change (the hosted image's artifacts stage has no repository, so there it re-checks
the bytes, hashes and metadata only, with `TREER_ARTIFACT_VERIFY_NO_GIT=1`; the
ancestry and allowlist check ran on the host in `stage-hosted-proxy.sh`), and bytes altered after the copy fail the recorded hashes even if the
checksum files were rewritten. The Canary manifest records
`machine_artifacts: {source_commit, reused, files}` for every release (the same
25 hashes), the Proxy serves the reused bytes, and installed binaries and the
embedded launcher keep reporting the commit they were actually built from. The
trust base is the operator's local `.treer/canary-checkouts/` state, as for any
collected `dist/`: the recorded hashes pin what was verified at copy time, but
they are not a signature or an independent pin of the previous release's bytes. `mobile/` is not on the allowlist, so a change there builds natively. The
Proxy serves only the three binaries and `SHA256SUMS` per platform, so the
provenance file is never downloaded by clients, and the installed binaries'
reported commit (the build commit) is not treated as a downgrade of the
candidate. The log names the chosen source and the number of changed paths, and
the manifest records whether reuse was enabled. Set `TREER_REUSE_NATIVE_ARTIFACTS=0` to
force a native build. The hosted Proxy and the Cloudflare Worker are always built
or deployed from the candidate, so changes to `apps/policy` (embedded in the
Proxy) and to the frontend still ship.

## Workspace Apps

Both Proxy Docker build contexts include the Policy client and all other embedded
Policy assets. The release tooling gate checks these files against the source
`include_bytes!` references before an image build.

Mail and Telegram are source Apps, not additional Rust machine binaries and not
part of the Cloudflare control-plane artifact. Deploy them from the same clean
source commit, build Mail's `web/dist`, and record the revision. Configuration,
Python bytecode, channel secrets, and App state are never release artifacts.
App supervision, rollback, state migration, and isolation use the deployment's
normal service tooling; Treer has no App package installer.

## Graded gates and staging batches

`python3 scripts/release-grade.py <base> <head>` classifies a candidate by the
paths it changes and prints the gates it must pass (`--json` for tooling,
`--urgent` for a release that must never be batched). It only plans; it runs no
gate, pushes nothing and deploys nothing. Any path that is not listed below,
including a new top-level directory, classifies as core, so a new area can only
make a release stricter.

| Changed paths | Local gate | Native build, Canary and Policy check |
| --- | --- | --- |
| `docs/`, root `*.md` only | `node scripts/check-docs.mjs` | not required |
| `apps/` only | docs check plus each changed App's own tests and `npm run check` | not required; the App owner deploys the App after CI |
| `web/`, `mobile/` | web typecheck, build and Playwright | Canary and the Policy check required; for `web/` changes the native build is reused from a previous `canary_passed` commit when [allowed](#reusing-native-artifacts-for-web-apps-and-docs-changes) |
| `launchers/` | `just agent-launcher-test` | required: the Controller embeds the launcher payload |
| `scripts/`, `.github/`, `justfile` that are not part of the release pipeline | `just check` | not required |
| release pipeline: canary, deploy, promote, artifact staging/verification, machine-update scripts and `build-release-artifacts.yml`/`publish-ghcr.yml` | `just check` | required; ships alone |
| `crates/`, `deploy/`, `canary/`, `native/`, `skills/` (embedded in the CLI), build files, unknown paths | `just check` | required; ships alone |

A mixed change takes the union of its components' gates, and any core path
makes it a core release. `.gitattributes` and `.gitignore` alone do not raise the
grade. Graded gating changes only which local gate and which shipping steps
apply. `just check` itself is unchanged, CI still runs every job on the exact
SHA, a skipped gate is never replaced by a rerun, and nothing here lets a
candidate reach Canary without the exact-commit native artifacts that
`just canary` already requires.

Frozen branches that their owners and the required reviewers have approved can
be released together instead of one Canary per branch:

1. Merge approved web, launcher and apps branches into a staging integration
   branch based on `origin/main`, one merge per branch, recording each source
   commit and proving by `git diff` that the staged tree is the union of the
   sources.
2. Cut one candidate from staging, run the graded local gate for the union of
   its paths, push the exact SHA to `main`, then run CI, the native workflow and
   `just canary` once for the whole batch. Record each step on the batch's
   release ticket.
3. Machine upgrades take the whole batch at once. Launcher changes always travel
   inside the Controller build, so a batch that contains one is upgraded through
   the normal Controller update, not a separate launcher path.

A batch is cut when the leader asks for it, when approved work has waited a day,
or when a release candidate has accumulated enough approved branches that
testing them separately would cost more than testing them together. These rules
do not batch:

- Security fixes and blocking bugs release alone and immediately.
- Core changes (`crates/`, authentication, Policy, migrations) and changes to the
  release pipeline scripts release alone so that a failure points at one change.
  Plain tooling changes may share a release with other tooling or docs.
- A branch whose review is incomplete or whose tests are red waits for the next
  batch; it is never carried in with a note.

When a gate fails and passes on its single permitted rerun, the release does not
continue until a ticket labelled `flaky` exists, assigned to the component
owner, containing the failing job, the test name, the exact SHA, the CI run and
the saved log path. A rerun that passes is evidence of flakiness, not of
correctness.

## Release tickets

Every release candidate, review and acceptance is its own Issues ticket, and the
order between them is `blocked_by`, not a chat instruction. One `[Release]`
ticket (label `type:release`, assigned to the release owner) carries one comment
per step in the form `[release] <step> <SHA> <result>` for integrate, check,
push, CI, native, canary and Policy check, quoting the exact full commit and the
log or receipt path. Each required reviewer gets a `[Review]` ticket that blocks
the release, and each real-target acceptance (for example an official Windows
Controller) gets an `[Acceptance]` ticket blocked by the release. A gate that
fails once and passes on its permitted rerun additionally gets a `flaky` ticket
(see [Graded gates](#graded-gates-and-staging-batches)).

When a Canary candidate reused native artifacts, the machines run binaries that
report their real build commit, not the candidate's. The Policy check therefore
takes that commit explicitly (`--controller-commit`, the manifest's
`machine_artifacts.source_commit`), and the receipt records it; for a fresh native
build it stays the candidate commit.

`python3 scripts/release-tickets.py --name <release> --parent <epic>
--release-owner <id> --review ROLE=<id> --acceptance ROLE=<id>` prints the
`issues create` commands in dependency order; add `--create` to create them. The
script uses the Issues CLI's own identity and never reads or stores credentials.

## Canary release

For routine releases, use the wrapper from the development checkout:

```bash
just canary HEAD --dry-run
just canary HEAD --push
# Retry the exact commit after fixing a failed build/deployment:
just canary <full-commit> --resume
# Optional completion message; requires the actual managed caller to match:
just canary HEAD --push --notify leader --agent /treer-dev-team/release-eng
```

The wrapper resolves the revision once and creates a detached worktree under
`.treer/canary-checkouts/<commit>/`, leaving concurrent development untouched.
`--push` only fast-forwards `origin/main`; divergent history requires a reviewed
merge. It installs frontend dependencies and runs the maintained release pipeline
below, including all five native artifact builds. It preserves the worktree and
release manifest for inspection and retry. `--dry-run` only prints the local
plan; it performs no writes or network calls. The wrapper never updates local
CLI binaries, restarts a local Host/Controller, or promotes Production.

Before it starts any release step the wrapper refuses to publish unless the exact
commit has a completed successful `CI` run (push or manual dispatch; a pull-request
run or a run for another commit does not count, the latest run decides, and it waits
up to `TREER_RELEASE_CI_TIMEOUT` seconds, 2700 by default) and its native artifacts are
green: a successful `Machine artifacts <commit>` run for that exact revision (the run
title decides, so a dispatch on this ref that built another revision does not
count; the run receipt must belong to the candidate and, when the candidate's
workflow defines it, exactly one "Native Windows embedded launcher and ACP UI" job must be
completed and successful; an existing `dist/` is still verified against it), or a verified reuse
anchor (`scripts/reuse-machine-artifacts.sh --check-only`, or `--verify-dist` for a
`dist/` that already carries reuse provenance). A local `dist/` alone is never
evidence of a green native build. All `gh` calls are time-bounded. Dispatch
the native workflow and let CI finish before running `just canary`;
`python3 scripts/canary-release.py <commit> --check-ci` waits for CI alone.

`--resume` reuses the completed local gate only when its receipt matches the
exact commit and unchanged frontend SHA-256. Missing or mismatched receipts
rerun the full gate; deployment, artifact verification, and two-machine tests
always run again. These receipts are local operator state, not signed build
attestations. Native workflow failures remain failures: inspect the failed job
and explicitly rerun it with `gh run rerun <run-id> --failed` before resuming.
No failing test is skipped or automatically retried by the wrapper.
The deployed frontend must match the candidate HTML byte for byte except for the
Worker's per-response `nonce` attribute inserted into real script start tags.
The gate reconstructs that exact transformation from the candidate and requires
each script nonce to match the response's nonce-only CSP; arbitrary normalization,
changed script content and other HTML changes are rejected.

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

### Windows manual machine update gate

`scripts/update-machine.ps1` is the operator-reviewed Canary checksum workflow,
not the signed release-channel updater. Prepare requires `-SumsSha256` from the
trusted, independently delivered Windows release receipt; it pins the HTTPS
manifest before parsing or executing downloaded binaries. This is an out-of-band
integrity pin, not a publisher signature. It needs only Windows
PowerShell 5.1 or PowerShell 7 and Windows/.NET facilities, not git, Python, npm
or a Unix shell. See the operator guide for split steps and receipts.

The independent `windows-launcher-smoke` job is a required promotion gate.
It downloads the already uploaded immutable Windows artifact, checks the
Controller copy against that artifact's SHA256SUMS and runs browser/ACP tests
without a write-back path to release bytes. Artifact collection requires the
whole workflow and this job to succeed, including when an explicit run ID is
supplied. Record its job/run link, `windows-smoke.json`, screenshots and the
Playwright lockfile's Chromium revision in the release receipt.

Next-version Windows acceptance must actually run Node 22, PowerShell crypto
and tasklist through exec and through App/Agent workload launch paths; verifying
environment maps alone is insufficient. The artifact job runs the update
transaction fixtures under both PowerShell shells, including real running-image
rename and private DACL assertions, with the candidate's native binaries.
Launcher installation plus a fake ACP Agent with its registered UI must also
pass in a closed native Windows fixture. Pending native tests are not evidence
of success; a failing job blocks release rather than being retried into green.
The operator then updates win_native explicitly and checks its App/UI Agent;
release automation must not update that machine or restart existing Agents.
