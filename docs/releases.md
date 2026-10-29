# Release process

- Status: maintained
- Last reviewed: 2026-10-28

A release is an exact Git commit on `main`. The same commit is built once per
platform by CI, deployed to the hosted Canary, then to Production, tagged for
self-host images, and optionally signed into the R2 update channels.

```bash
just check                    # everything CI runs, locally (needs `just test-db-up`)
just deploy canary            # hosted Canary: Railway Proxy + Cloudflare App
just test-canary              # two-machine black-box test against Canary
just deploy production        # same clean commit, must be in origin/main
git tag vX.Y.Z && git push origin vX.Y.Z   # GHCR self-host images + tag artifacts
just artifacts-prepare vX.Y.Z && just artifacts-canary vX.Y.Z   # signed R2 channel
just artifacts-stable vX.Y.Z
```

## Hosted platform

Railway runs the Proxy replicas, PostgreSQL, NATS and the two Canary test
machines. Cloudflare Workers Static Assets serves the React App; only `/health`
and `/config.json` run Worker code, so one bundle works for any Proxy origin.

| Environment | App | Proxy |
| --- | --- | --- |
| Canary | `https://app.canary.treer.ai/` | `https://proxy.canary.treer.ai/` |
| Production | `https://app.treer.ai/` | `https://proxy.treer.ai/` |

`scripts/deploy-hosted.sh canary|production` (`just deploy`) requires a clean
worktree and authenticated `railway`, `wrangler` (through `pnpm`), `gh`, `jq`
and `curl`. It:

1. puts verified native machine artifacts for `HEAD` into `dist/`
   (`scripts/fetch-machine-artifacts.sh`, see below);
2. uploads `git archive HEAD` plus `dist/` to Railway with
   `railway.json` rewritten to `Dockerfile.hosted`, which bundles all fifteen
   machine binaries without recompiling them and re-verifies them in the image;
3. builds `web/` and runs `wrangler deploy --env <target>`;
4. waits for the Railway deployment, then checks Proxy health, every served
   `/artifacts/<platform>/<binary>` byte for byte without redirects
   (`verify-served-artifacts.sh`), App `/health` and `/config.json`, and the
   served App HTML against the local build (`verify-served-frontend.py`);
5. on Canary checks the wildcard ingress certificate; on Production the wildcard
   404 probe. Production also refuses a commit not contained in `origin/main`.

Railway IDs and URLs default to the Treer project; override with
`TREER_RAILWAY_PROJECT_ID`, `TREER_PROXY_SERVICE`, `TREER_RAILWAY_ENVIRONMENT`,
`TREER_PROXY_URL`, `TREER_APP_URL`, `TREER_INGRESS_URL`,
`TREER_WORKER_ENVIRONMENT` and `TREER_DEPLOY_TIMEOUT`. `just test-canary` and its
fixtures are described in [Canary](#canary).

Optional Cloudflare Workers Builds may upload inactive Canary App versions from
`main` (`worker:upload:canary`); uploads never receive traffic.

Roll back the App with `wrangler rollback --env production <version-id>` and the
Proxy by redeploying its previous Railway deployment. Database contraction is
never part of a code rollback: use expand-first PostgreSQL changes so old and
new Proxy replicas can overlap, and add Proxy API fields before the App uses them.
A Controller whose startup store is v2 (reservation fences) cannot roll back to a
Controller without `agent.reservation-fence.v1`; never delete fences to force it.
Back up App state (Mail, Telegram) before replacing an App; stop App processes,
not Core Message routes, to stop channel traffic.

## Canary

Canary is a separate Railway environment (Proxy, PostgreSQL, NATS), a separate
Worker and the wildcard ingress domain `*.canary.apps.treer.ai`. A revision is
promoted only after `just test-canary` passes; health checks alone are not
enough. The wildcard needs DNS-only records that Railway reports with
`railway domain status` (`CNAME *.canary.apps`, `CNAME _acme-challenge.canary.apps`,
and a `_railway-verify` TXT while unverified).

`just test-canary` (`scripts/test-canary.sh`) is a black-box test against the
deployed Proxy. It logs in with the Canary admin password, reuses the
`canary-tester@treer.invalid` account and `canary-e2e` workspace, restarts the
two long-lived Railway machine services `treer-canary-machine-a`/`-b`
(enrolled as `canary-a`/`canary-b`, image in `canary/machine`, identities on
their `/workspace` volumes), and checks:

1. machine B's loopback HTTP server, registered as a service, is reachable from
   a command Agent on machine A through `treer service forward`;
2. the same service published on wildcard HTTPS is reachable from the tester;
3. the A-to-B payload appears in traffic accounting.

It then deletes only the resources it created. Variants:
`just test-canary-provision` redeploys the machine image to both services;
`just test-canary-enroll` additionally mints new enrollment keys (only after an
identity was removed). `TREER_CANARY_KEEP_RESOURCES=1` keeps the run's
resources for investigation; `TREER_CANARY_SKIP_PUBLIC=1` skips the ingress
step and is not Canary evidence. Cookie writes send `Origin` from
`TREER_CANARY_APP_URL`. Other overrides: `TREER_RAILWAY_PROJECT_ID`,
`TREER_CANARY_ENVIRONMENT`, `TREER_CANARY_PROXY_SERVICE`,
`TREER_CANARY_MACHINE_{A,B}_{SERVICE,NAME}`, `TREER_CANARY_TEST_TIMEOUT`,
`TREER_CANARY_RESTART_ATTEMPTS`. The test does not exercise Mail, Telegram or
other Apps.

Cloudflare preview URLs for uploaded Canary Worker versions are public but take
no traffic from `app.canary.treer.ai`; they are frontend build evidence only.

## Machine artifacts

`Build release artifacts` (`.github/workflows/build-release-artifacts.yml`) runs
for `v*` tags or by manual dispatch with a `revision`. Native runners build the
CLI, Host, Controller and the `treer-launch` Agent launcher (its own Cargo
workspace, built with `--target-dir target`) for `linux-x86_64`,
`linux-aarch64`, `darwin-x86_64`, `darwin-aarch64` and `windows-x86_64`. `scripts/stage-release-artifacts.sh`
(`.ps1` on Windows) stages each platform with `SHA256SUMS` and
`build-metadata.json`. Windows also runs ConPTY/Controller/CLI tests, the
standard-user and installer checks and a byte-identical clean rebuild (`/Brepro`).
The workflow never publishes anything; the Windows launcher is checked manually
on a `win_native` machine before release.

`scripts/fetch-machine-artifacts.sh [revision]` (`just fetch-artifacts`) verifies
an existing `dist/`, or downloads the newest successful run for the exact commit
(`TREER_ARTIFACT_RUN_ID` selects one), dispatching the workflow if no run exists.
It rejects missing
platforms, mismatched commit metadata, partial checksum files and missing
executable bits (`scripts/verify-machine-artifacts.sh`). A stale `dist/` fails;
delete it to fetch another commit. `just stage-artifacts` builds only the local
platform for testing.

Windows artifacts keep the logical names without `.exe`; `scripts/install.ps1`
saves them with the suffix and fails closed without `SHA256SUMS`.

## Self-host images

Pushing a `vX.Y.Z` tag that matches `Cargo.toml` runs `Publish GHCR images`:
native `linux/amd64` and `linux/arm64` images for `treer-proxy` (`Dockerfile`),
`treer-app` (`web/Dockerfile`) and `treer-updater` (`deploy/updater/Dockerfile`)
with immutable `vX.Y.Z` and `sha-<commit>` tags and a moving `canary` pointer.
After Production, dispatch the workflow with `channel=stable` and the version;
`scripts/retag-ghcr.sh` retags existing digests without rebuilding. `latest` is
never published. Compose pulls these images; see [Self-hosted Treer](../deploy/README.md).

The same artifact workflow also stages `treer-proxy` for both Linux platforms
(`stage-release-artifacts.sh --proxy`), assembles `.single-container-input/`
with `scripts/assemble-single-container-input.py`, builds `Dockerfile.single`
from that exact input digest on each architecture and runs
`deploy/single-container/smoke.mjs`. That image is not pushed; operators build
it locally with `scripts/build-single-image.sh`.

## Signed R2 releases

Installed Controllers update only from signed releases in the `treer-releases`
R2 bucket at `https://releases.treer.ai/`. Version objects under
`releases/<version>/` are immutable; `channels/canary.json` and
`channels/stable.json` are separately signed pointers. Every manifest and
pointer carries a detached Ed25519 signature, and every binary is pinned by
size and SHA-256.

`scripts/release-r2.mjs` (`just artifacts-*`) needs all three binaries for every
platform in `dist/<platform>/` (`TREER_RELEASE_PLATFORMS` narrows the set), a
clean worktree, and a version equal to `Cargo.toml`:

```bash
just artifacts-prepare vX.Y.Z
just artifacts-canary vX.Y.Z
just artifacts-verify vX.Y.Z
just artifacts-stable vX.Y.Z    # local tag must point at the canary manifest's commit
```

Promotion never re-uploads binaries; republishing identical bytes is
idempotent and different bytes for an existing version are rejected. Uploads
use Wrangler; `TREER_RELEASE_BUCKET`, `TREER_RELEASE_BASE_URL`,
`TREER_CLOUDFLARE_PROFILE` and `TREER_WRANGLER` select another environment.
Publishing does not update machines; see
[Updates](operator-guide.md#machines).

## Official release signing authority

The public SPKI in `keys/official-release-ed25519.pub.pem` is the authority for
`https://releases.treer.ai/`; its DER SHA256 is
`634761ff47a6a113b4b8925f0db0e030d8bfce07f2fa228fdc0a4904874dc41f` and
`keys/official-release-authority.json` records its designation. New Controllers
embed the same key.

The private key lives in the publisher's macOS login Keychain as a
non-synchronizing item with an explicit signer-executable ACL; it never exists as
a PEM file, argv, Git object or stdout. Build `scripts/release-keychain.swift`
with `xcrun swiftc -O` into a stable owner-only directory (mode 700) and record
its SHA256. `status`, `public`, `sign` (≤ 1 MiB stdin, detached signature) and
`selftest` are routine; `create` establishes the authority once and refuses an
existing item. There is no export. Pass `--keychain-signer ABS --signer-sha256
HASH` to `release-r2.mjs prepare|publish|promote`; it pins the executable, checks
its key against the repository SPKI and verifies every returned signature
(`scripts/release-keychain.mjs`). Self-hosted publishers may use an explicit key
file instead, never together with Keychain custody.

For an existing installation without the key,
`node scripts/install-release-trust.mjs [--config-dir ABS]` installs only the
shipped public key (exclusive create; refuses a different or unsafe existing key).

## Launcher

`treer-launch` is an ordinary artifact of every signed release, listed in the
manifest beside the other executables; there is no separate launcher channel.
`treer-launch update` (`treer-agent-server launcher update`) installs only that
artifact, by default at the installed Controller's version. Consumers treat a
manifest without `treer-launch` as predating it.

## Release notes

### 0.2.2

Additive release: no schema contraction. The Proxy adds columns to
`invitations` (`invitation_id`, `email`, `expires_at`, `revoked_at`,
`created_by_kind`, `created_by_name`, `group_ids`, `workspace_ids`,
`explicit_role`), `sessions` (`session_id`, `last_seen_at`) and
`app_deployments` (`ui_capabilities`), backfilling IDs for existing rows; a
0.2.1 Proxy keeps working against the expanded schema. Update machines from the
Console or with `treer-agent-server update --version v0.2.2`. Running launcher
Agents keep their old browser UI until they are restarted.

- **Trust model:** Agents are trusted collaborators and Policy is
  cooperative. Agent administration is gated by ordinary Policy evaluation,
  not by grants, caps or defences against malicious Agents or Policy
  authors. Tenant isolation, human authentication, the last-owner invariant
  and actor audit are unchanged.
- **Console settings hub:** a top bar with an organization/workspace context
  switcher and Cmd/Ctrl+K search; settings pages for You (profile and password, sessions,
  preferences), the organization (members, invitations, groups, danger zone) and
  the workspace (access and effective access, machines, policy, audit, danger
  zone) on real routes (`/settings/*`, `/orgs/:o/settings/*`,
  `/orgs/:o/workspaces/:w/settings/*`), so Back and deep links work. The
  sidebar shows only the workspace (Machines collapsed by default, a
  Workspace settings link) and the sidebar "Needs attention" inbox is gone;
  Tree status icons and the Overview board's Needs attention section remain.
- **Launcher UI redesign** (`treer-launch`, same HTTP/WebSocket API): worded
  run status in the header and tab title, per-turn activity summaries and
  changed-file diffs, a run bar and a stable composer with explicit
  Steer/Queue, Files and Tasks drawers, and a recovery banner that keeps the
  conversation visible during a provider restart. One light/dark design
  replaces the three looks. The conversation now stops following only when
  the reader scrolls up (a hidden Console tab or layout reset no longer
  leaves it stuck at the top).
- **Launcher model selection on resume:** the chosen model and effort are
  journaled and re-applied after a provider or launcher restart (Claude via
  `model/set`/`effort/set`, Codex via the thread model); previously a restart
  silently fell back to the default model.
- **App UI capabilities:** Apps declare browser UI contracts with
  `treer app create|update --ui-capability NAME` (repeatable) or
  `ui_capabilities` on the App create/update API; `AppDeployment.ui_capabilities`
  persists them. `app-tree.v1` makes the Console show the App's subtree without
  probing; Agent Hub and External Agent declare it. A capability-only update
  does not restart the App and needs `agent.update`.
- **Agent administration through Policy** (`agent.admin.v1`): Agents call
  `$TREER_AGENT_SERVER_URL/api/org-admin/...` (Proxy
  `/agent/workspaces/{ws}/admin/...`) to manage their own workspace and its
  organization: invitations, members and roles, groups, workspaces, access,
  enrollment, audit and the Policy Provider. The actions `org.manage`,
  `org.member.manage`, `org.invite.manage`, `org.group.manage`,
  `workspace.manage`, `workspace.access.manage`, `policy.provider.manage`,
  `audit.read` and `machine.enroll` are ordinary Policy actions (allowed
  unless an enforced rule or default denies them); Agents are trusted
  collaborators and Policy is cooperative. Agents never reach another
  organization, the last owner is kept, and deleting or transferring the
  organization stays human-only. Changes are audited with the Agent as actor;
  its name is frozen at write time and the Console shows `agent: <name>`.
  Within its own organization an Agent acts like an owner: any invitation
  role, role changes and owner removal.
- **Invitations:** `email` is where the link is delivered, not a binding;
  any signed-in account with a usable link may accept. Invitations stay
  usable until accepted, revoked or expired. `POST /api/auth/password` now
  rotates the current session token and returns `200` with the new session
  cookie.
- **Fixes:** deleting an organization works after its workspaces metered
  traffic (meters and network receipts are removed with it); workspace IDs
  on create (human and Agent) must be 1-128 ASCII letters, digits, `_` or
  `-` (`400 invalid_workspace_id`); machine and Agent credentials whose
  workspace row no longer exists no longer authenticate.
- **Settings APIs** (`settings.admin.v1`): invitations take `email`, `role`,
  `expires_in_hours`, `group_ids`, `workspace_ids` and can be listed and
  revoked (`GET`/`DELETE`); accepting applies them, and invitations created
  before this release still join as `member` and never expire. Organization
  `leave`, `transfer` and `DELETE` (`409 organization_has_workspaces` with
  `workspace_count`); owners can now promote/demote owners while one remains
  (`409 last_owner`). `GET/DELETE /api/auth/sessions`, `POST /api/auth/password`,
  `GET /api/workspaces/{id}/access/effective`, and
  `DELETE /api/workspaces/{id}` with `{confirm_name, revoke_machines}`.
  Schema: additive columns on `invitations` and `sessions`.
- **Console:** if `/api/health` advertises `settings.admin.v1`, an App without
  `ui_capabilities` is a leaf and is never probed for a tree; older Proxies are
  still probed.
- **Console:** machine-page service probes call
  `POST /api/workspaces/{ws}/services/{id}/probe` (the old path had no route).

### 0.2.1

Additive release: no breaking changes and no schema contraction. Update
machines from the Console or with `treer-agent-server update --version v0.2.1`.

- **Machine metrics** (`machine.metrics.v1`): Controllers sample whole-machine
  CPU, RAM and NVIDIA GPU load; the machine page shows them live and
  `treer machine metrics <machine> [--window 15m]` returns a window. Machines
  on older Controllers report `machine_metrics_unsupported`.
- **App email restored** (`app.email.v1`): Managed Apps send through the
  Proxy's Cloudflare sender with `treer email send` or
  `POST /api/apps/{service_id}/email`, governed by `email.send`, and look up an
  earlier send with `treer email status KEY`. It reuses the
  `app_email_*` tables 0.2.0 left in place. The recipient is `to` (any address)
  or `to_member` (`--to-member USER_ID`: Core resolves the member's verified
  account email; `email_member_not_found` 404, `email_member_unverified` 409).
- **Mail App: Agents email people** (`POST /api/email` with `to_member` or
  `to`, `subject`, `text`, `message_id`): Mail emails the person the Agent's
  text plus a reply link that opens the Core thread in its web inbox after
  sign-in; `GET /api/email/KEY` wraps Core's status. The unread-Message email
  notifications (settings, digests, Notifications view, `email_preview_chars`,
  `notify_poll_seconds`) are removed; Mail drops their SQLite tables on start.
- **Per-endpoint App inbox** (`message.endpoint-inbox.v1`): `app_endpoint` and
  `after` on Message receive/ack (`treer message receive|ack --endpoint KEY`).
  Additive; needs a Proxy and Controller that both advertise it.
- **Apps:** Issues without process machinery (open edits, hide/restore, Agent
  wake-ups) and the External Agent App (one CLI path, generic endpoint inbox).

### 0.2.0

Breaking changes; read before upgrading a workspace or machine:

- **Controller update protocol v2.** Controllers self-update by downloading a
  signed release, replacing their executables and restarting the Host. A
  machine on 0.1.x cannot follow the new protocol from the Console: run
  `treer-agent-server update` once on each machine (or `update --version
  v0.2.0`); later updates work from the Console again.
- **Agent launcher renamed `treer-launch`.** It is a self-contained Rust binary
  shipped in every signed release and installed or updated with Treer
  (`treer-launch update`). `treer-agent` remains as an alias; the Node
  launcher and its installer are gone.
- **ACP profiles removed.** Only the Codex and Claude harnesses of
  `treer-launch` are supported; recreate ACP Agents with one of them.
- **Removed features:** Voice, Console provider CLI install, and Managed App
  email. Their Proxy tables (`controller_update_*`, `provider_cli_*` and
  `app_email_*`) are no longer used but are left in place by 0.2.0; a later
  release drops them, so back them up before that upgrade if you need history.
- **Launcher auto-recovery.** The launcher no longer stops and waits for a
  human: a failed provider is restarted and the same session resumed with
  capped backoff and no attempt cap; an in-flight prompt is settled from the
  resumed history and never replayed when history shows it was delivered; the
  queue is never paused. The manual `acknowledge_unknown` recovery action and
  the private-store VCS placement rule are removed. See
  [launchers/agent/README.md](../launchers/agent/README.md).
- **Re-enrolling replaces stale configuration.** `treer-agent-server connect`
  on a machine that already has a registration for the workspace replaces it
  when the root or Proxy changes or the Proxy no longer accepts the old
  machine identity (for example, a machine deleted in the Console). Running
  workloads on the old Host need one confirmation (or `--replace`).
  `service relocate-root` is removed.
- **Windows:** Treer installs and runs as a standard user, like Unix (the
  Administrators-group rule and `windows_requires_administrator` are gone;
  Host and registration locks are per-user file locks). `treer-launch` runs
  the same journal, history/recovery, file browser (directory listing
  included), image attachments and task-output code as on Unix, and finds
  npm/pnpm/yarn `.cmd` provider shims on PATH without `CODEX_BIN`/`CLAUDE_BIN`.
- **Legacy Node state import removed.** `--import-node-state`,
  `--import-claude-state` and `--restore-node-backup` are gone; state left
  under `<cwd>/.treer/agents/` by the Node launcher is ignored.
