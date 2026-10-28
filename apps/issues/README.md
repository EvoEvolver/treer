# Issues App

Issues is a workspace App for humans and Agents sharing a tracker: issues with
Markdown source descriptions, priorities, labels, assignments, parent/child
hierarchies (a DAG), blocked-by dependencies, comments, activity, notes and
leased claims, plus change notifications and reminders sent as Core Messages.
It does not schedule or prompt Agents.

The HTTP root negotiates the [Agent manual](AGENT.md) (commands and complete
`/v1` API) or the human UI. This README covers deployment and operator concerns.

## Design

The backend is Python 3.10+ standard library and SQLite, with no Node at
runtime. The React/TypeScript UI in `ui/src` is built with pinned esbuild into
committed files in `dist/`, served as-is. Treer owns identities, service tokens
and browser OAuth; there are no local users. All state, including OAuth session
mappings, is in one SQLite database. Writes use `BEGIN IMMEDIATE`, so permission
and claim checks run in the same transaction as the mutation; references and
parent/dependency cycles are validated before commit.

APIs use `/v1/`, bounded pages and JSON errors. Assets and API calls are
document-relative and preserve a Treer tunnel prefix. Descriptions and comments
are displayed as **text** (no Markdown rendering, no HTML sinks), and the CSP
allows only same-origin scripts and styles.

### Interface

A dense, keyboard-first layout with light/dark themes. Issues are grouped by
status (done/closed collapsed) or shown as a hierarchy tree, with status,
assignee, label and text filters; the detail pane edits status, assignee and
priority in place. On phones the detail is a full-screen sheet. Views include
**Needs me** (`GET /v1/issues?assignee=me&unresolved=1`) and **Stale**. Close /
reopen is one click with an Undo toast; buttons are disabled with the reason
when the viewer may not edit (the server still enforces permissions).

Assignee pickers show humans under People and Agents in folders by the canonical
`path` from Core's permission-filtered directory. A missing path (`null`, hidden
or unplaced) shows the stable ID. Selection always sends the stable principal ID;
paths are display-only and never used for authorization.

Shortcuts: `J`/`K` move, `Enter`/`O` open, `X` close, `E` edit, `C` new issue,
`/` search, `Esc` back, `?` help.

### Building the UI

```sh
cd apps/issues
npm ci --ignore-scripts
npm run build:ui     # rewrites dist/
npm run check        # tsc, rebuild-and-compare, size budget, no absolute paths or HTML sinks
```

`dist/` must match a fresh build byte for byte; `.gitattributes` forces LF for
`ui/` and `dist/`.

## Deploy as a Managed App

Do not use `--public`:

```sh
treer app list
treer app create --machine self --name issues --cwd treer --port 9440 \
  env -- \
  ISSUES_PROXY_URL=https://proxy.example.com/ \
  ISSUES_DB=/absolute/durable/path/issues.sqlite3 \
  python3 apps/issues/issues.py
treer app show /issues
curl -fsS http://issues.treer/health
curl -fsS http://issues.treer/v1/me
```

`ISSUES_PROXY_URL` is the deployment's public Proxy URL (not the App ingress or
Controller loopback URL). A logged-in Treer user is recognized automatically
inside the Console and through Workspace ingress; the OAuth button appears only
for direct access without a forwarded identity. The App discovers its own
service, workspace and public URL with `treer app list` and its runtime
identity (its Policy must allow App listing); discovery failure returns 503,
never anonymous access. Without wildcard ingress it uses the
`/api/workspaces/WORKSPACE/apps/APP_ID/ui/` root; the OAuth fallback callback
still needs wildcard ingress. For externally supervised servers, or a Policy
that denies App listing, set `ISSUES_SERVICE_ID`, `ISSUES_PUBLIC_URL` and
`ISSUES_WORKSPACE_ID` explicitly.

Use the App's stable ID or tree path (for example `/infra/issues`) with
`treer app` commands.

### Version-directory deployment

Issues supports `scripts/app-deploy` (see "Deploy new Apps from version
directories" in the [operator guide](../../docs/operator-guide.md)): a stable
entry runs `releases/<FULL SHA>` selected by `current`, with the database in
`ROOT/state/app.sqlite3`. For this the App provides:

- `GET /deployment-health` (unauthenticated, `no-store`):
  `{"version": APP_DEPLOY_VERSION, "app_id": TREER_AGENT_ID, "ready": …, "database_ready": …}`,
  503 when the database does not answer, cached for two seconds.
- `--proxy-url`, `--leader-agent` and `--release-agent` flags, because the saved
  command cannot carry environment variables; pass them as fixed arguments to
  `app-deploy init` (`… --db-env ISSUES_DB -- --proxy-url https://proxy.example.com/`).
  The entry sets `ISSUES_DB` and `APP_DEPLOY_VERSION`.

`init` records the interpreter path as given; pass a stable shim (not a
version-numbered path) and keep it writable only by trusted operators.
`tests/rehearse_migration.py` rehearses moving an existing App onto this layout
on a copy, printing each command and its rollback point.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `ISSUES_PROXY_URL` / `--proxy-url` | required | Trusted Proxy origin for verification, directory, OAuth and Messages |
| `ISSUES_LEADER_AGENT` / `--leader-agent` | unset | Stable Agent ID (`ag_…`) that `leader`/`user` in `waiting_on` resolve to and that receives ownerless-issue reminders; unset refuses those values and skips ownerless reminders |
| `ISSUES_RELEASE_AGENT` / `--release-agent` | unset | Agent whose release comments trigger shipped-but-open reminders (below); off when unset |
| `ISSUES_LISTEN` | `127.0.0.1:9440` | Listener; match the App's `--port` |
| `ISSUES_DB` | `.treer/apps/issues/issues.sqlite3` | SQLite file, relative to the runtime cwd if not absolute |
| `ISSUES_SERVICE_ID` | own Managed App record | Service audience; set together with the public URL |
| `ISSUES_PUBLIC_URL` | own Managed App record | Workspace-authenticated App origin with trailing slash; also the link in human notifications |
| `ISSUES_WORKSPACE_ID` | injected or own record | Workspace boundary; required for external supervision |
| `TREER_BIN` | `treer` | CLI for discovery, `agent show` and App-identity Messages |
| `ISSUES_URL` | `http://issues.treer/` | Root used by the `issues` client |
| `ISSUES_NOTIFY` | `on` | `0`/`off`/`false`/`no` disables change notifications and digests |
| `ISSUES_NOTIFY_HOURLY_LIMIT` | `20` | Immediate Messages per recipient per hour (1–1000) |
| `ISSUES_REMINDERS` | `on` | `0`/`off`/`false`/`no` disables the reminder poller |
| `ISSUES_REMINDER_SKIP_LABELS` | `epic,pinned` | Labels (case-insensitive) never reminded; empty = none |
| `ISSUES_REMINDER_FIRST` | `10m` | First reminder delay (1m–24h) |
| `ISSUES_REMINDER_FACTOR` | `2` | Backoff multiplier (1–10) |
| `ISSUES_REMINDER_MAX` | `24h` | Interval cap (≥ FIRST, ≤ 30d) |

Durations are written `90s`, `10m`, `1.5h`, `24h`. Invalid values stop startup
with the variable named; they are never replaced by defaults.

## Authentication

Authentication checks an injected `X-Treer-Identity-Token` first (even if a
cookie exists), then explicit Bearer, then the OAuth cookie. Invalid, expired or
other-audience supplied tokens fail without cookie fallback, and identity is
reverified on every call. OAuth uses S256 PKCE, one-use state paired with an
HttpOnly browser nonce, hashed session IDs and HttpOnly/SameSite (Secure on
HTTPS) cookies; the UI never receives tokens. Human writes require JSON and
`X-Issues-Request: 1`; a present `Origin` must match the App or Proxy origin and
`Sec-Fetch-Site` must be same-origin or same-site. No CORS is enabled.

## Permissions

Every authenticated workspace principal may read, create and comment. Only the
creator, assignee, live lease holder or a human owner/admin may edit; a live
lease restricts mutation to its holder (anyone may still comment). Only
unassigned issues or issues assigned to the caller can be claimed; leases last
1–86400 seconds and renew/release need a live owned lease. Dependencies must be
resolved before claiming, starting or marking done. Human admins can edit after
a lease expires but cannot steal it. This is a shared tracker, not per-issue
ACLs; Treer Policy remains an upstream gate.

## Notifications

Edits and comments notify the people they concern, never the actor, through
`/api/apps/SERVICE/messages` using the **verified caller's** token, so Core
applies that caller's Message Policy:

| Event | Notified |
| --- | --- |
| Issue created, comment, other edits | assignee |
| Status or assignee change | assignee, creator, previous assignee on reassignment |
| `@name`, `@/tree/path` or `@ag_id` in a comment, or newly in a title/description | the mentioned Agent |

Mentions resolve with the author's own token (Core `recipients/resolve` and the
author's directory); at most 10 per event. Unknown or ambiguous mentions are
reported in `notification.unresolved_mentions` (with `mention_candidates` for
duplicate names). Only Agents can be mentioned. Claims and no-op updates send
nothing.

Messages contain a fixed sentence and a reference, never issue titles,
descriptions or comment bodies. The only author text is a comment title,
sanitized (`notify.clean_title`: NFKC, control/bidi/zero-width/private-use
removed, quotes and brackets neutralized, `@` → `at`, at most 120 code points)
and quoted behind `title (untrusted):`. Agents get the command
`issues show N --titles`; humans get the browser link when `ISSUES_PUBLIC_URL` is
set. Names appear only if they are 1–24 characters of `A–Z a–z 0–9 space . _ / -`,
otherwise the stable ID. Idempotency keys are `issues-event-ID` (and
`-human`). Delivery failures are recorded in activity; the write still succeeds.

**Intent and per-recipient policy.** A comment title starting with `[decision]`,
`[blocker]`, `[incident]` or `[action-<agent>]` sets its intent; anything else is
`fyi`. Each Agent's policy is `all` (default) or `asks`
(`PUT /v1/notify-policy {mode, agent?}` or `issues policy set asks|all [--agent ID]`;
callers change their own, human owners/admins any Agent's; audit at
`GET /v1/notify-policy?audit=1`). An `asks` recipient gets immediately only
decision/blocker/incident comments, `[action-X]` aimed at them, the issue becoming
`blocked`, or an assignment to them (at most 5 per author per hour); the rest
waits in one hourly digest per issue.

**Rate limits.** Per (recipient, issue, actor) the first event is sent at once
and opens a five-minute window; later events by that actor are coalesced into one
digest sent under the App's identity. Each recipient also has an hourly budget
(`ISSUES_NOTIFY_HOURLY_LIMIT`); excess updates fold into one overflow digest.
Because digests carry no caller token, only an actor whom Core already allowed
to message that recipient (in the window or current hour) can contribute to one;
others' events are dropped (`notification: throttled`). Failed digests are
retried at most three times. Pending digests persist in SQLite. The App's own
Policy must allow these Messages.

## Reminders

On by default. An issue needs a reminder while it is unresolved (not
`done`/`closed`/`note`), has no skip label, and has a responsible Agent: the
Agent it waits on (`waiting_on`), else its Agent assignee; an unassigned issue
goes to the leader Agent (if configured). Issues assigned to humans are not
reminded, and humans are never messaged.

- **Schedule:** per (recipient, issue) exponential backoff stored in SQLite:
  10m, 20m, 40m, … capped at 24h with the defaults.
- **Reset:** only progress restarts it: the responsible party commenting or
  editing, or responsibility moving to a party not responsible in the last 24
  hours.
- **Idle only:** at the due time the App runs `treer agent show ID` and sends only
  if the Agent is idle; busy or unreachable Agents are retried next poll without
  growing the interval. Polling is every minute.
- **Batched:** one Message per Agent per poll listing up to ten issue IDs and the
  command `issues list --assignee me --unresolved`, sent under the App's identity;
  at most 4 reminder Messages per recipient per hour. The idempotency key covers
  the issue set, versions and backoff steps; failed sends retry with the same key.

`waiting_on` is `null`, an Agent's stable ID, `leader` or `user` (both mean the
leader Agent). It exists only while the status is `blocked`, is set by anyone who
may edit, and records `waiting_by`/`waiting_at`. Set it with `issues wait`.

Anyone who may assign an issue to an Agent can thereby make the App message that
Agent; the text is fixed and still subject to Message Policy. The App's Policy
must allow `agent show` and Messages to the reminded Agents.

**Shipped-but-open reminder.** With `--release-agent` set, a comment by that
Agent titled `[release]` or `[deployed]`, containing `canary_passed` and a commit
of 8–40 hex digits, records one signal per (issue, commit prefix). The reminder
worker later tells the current Agent assignee once, when idle, that the issue
looks shipped and can be closed. Resolved or skip-labelled issues are settled
silently.

**Stale view.** `GET /v1/stale?hours=4&limit=50&offset=0` (hours 1..720;
`issues stale [--hours N]`) lists unresolved issues whose last progress is older
than that, oldest first, with the responsible Agent, `waiting_on`, `stale_hours`
and `since_any_activity_hours`.

## Search, references, hierarchy and notes

- **Search:** `GET /v1/search?q=…` (`issues search`, and Enter in the web box)
  over titles, descriptions and comments using SQLite FTS5 trigram (substring,
  case-insensitive, CJK from three characters; shorter words use a bounded
  `LIKE`). 1–8 words, ≤ 200 bytes; filters `status`, `assignee`, `label`,
  `author`, `from`/`to`; `limit` 1..50 of at most 500 candidates (`truncated`);
  2-second timeout (503); 30 searches per minute per principal (429). Hits name the
  verified author and a ≤ 160-character snippet marked `snippet_untrusted: true`.
  Everyone can search everything: never paste credentials into issues. The index
  is updated in the write transaction and rebuilt on start if missing.
- **References:** a standalone `#123` links to issue 123, which lists it under
  `referenced_by` (newest first, ≤ 50, with author). At most 20 links per text; no
  notification.
- **Hierarchy:** an issue may have several parents (≤ 50 parents and ≤ 50 children
  each, no cycles). Set `parent_ids` on create/edit (`parent_id` is a derived
  read-only compatibility value), or attach with
  `POST /v1/issues/PARENT/children {"add": [..], "remove": [..]}` / `issues link`
  / `issues unlink`. Adding needs edit rights on the parent; removing on the parent
  or the child. 30 added links per minute per principal. Parents never change
  their children's reminders, leases, dependencies or permissions. Progress
  rollups count each descendant once (`done`/`closed` complete, notes skipped).
- **Notes:** status `note` marks a knowledge entry: no reminders, not stale, not
  unresolved, excluded from rollups, treated as resolved in `blocked_by`.

## Agent client

Run `apps/issues/issues` (or download `/issues`, inspect it, and install it under
a user-owned bin directory). Python urllib honors the injected HTTP proxy, so
Agents use `http://issues.treer` without handling identity. Commands include
`list`, `show`, `activity`, `create`, `edit`, `comment`, `assign`, `status`,
`claim [--action acquire|renew|release]`, `children`, `link`, `unlink`, `wait`, `stale`,
`search` and `policy`; output and errors are JSON (errors exit nonzero). See
[AGENT.md](AGENT.md) for the full command and API reference.

## Backup and recovery

Use SQLite's online backup API, which captures committed WAL contents:

```sh
python3 - /absolute/durable/path/issues.sqlite3 /secure/backup/issues.sqlite3 <<'PY'
import sqlite3, sys, os
os.umask(0o077)
source = sqlite3.connect('file:' + sys.argv[1] + '?mode=ro', uri=True)
target = sqlite3.connect(sys.argv[2])
source.backup(target)
target.close()
source.close()
PY
```

Do not copy a live `.sqlite3` file alone. To restore, stop only this App
(`treer app stop /issues`), keep the old database and its `-wal`/`-shm` files,
put the backup at `ISSUES_DB`, remove stale sidecars and start the App. Schema
changes run on startup and are not reversible by downgrading code alone (for
example, parents move into `issue_parents`): restore the pre-upgrade backup
together with the old code. Do not run several App versions on one file.

Backups contain OAuth sessions and sensitive issue text; protect them. Startup
tightens the state directory to 0700 and uses private SQLite files. Same-UID
Managed Apps are not isolated; use a separate user or container/VM for critical
services.

## Verification

```sh
python3 -m unittest discover -s apps/issues/tests -p 'test_*.py' -v
(cd apps/issues && npm ci --ignore-scripts && npm run check)
cd apps/issues/tests
npm install --ignore-scripts
npx playwright install chromium
npm test
```

Playwright is test-only (`ISSUES_PLAYWRIGHT_MODULE=/absolute/module` reuses an
installed copy; screenshots go to `/tmp/treer-issues-screenshots` or
`ISSUES_SCREENSHOT_DIR`). Browser tests run the real server behind a local
gateway that mints 60-second human tokens, strips spoofed headers and rewrites
Host, covering Workspace ingress and the embedded
`/api/workspaces/ws_test/apps/app_issues/ui/` prefix at desktop and 390px, plus
the OAuth fallback. The gateway is a test double; Proxy signing is covered by
`cargo test -p treer-proxy`. Test processes clear inherited `TREER_*` and proxy
settings.

## Limits

Built for small trusted teams. Graph operations load workspace issue metadata
into memory. No attachments, email, kanban, deletion of issues or comments,
per-issue privacy, projects, cycles or saved views. Create and comment are not
idempotent: inspect after an uncertain response before retrying. Immediate
notifications are best effort after commit (no outbox). Claims are coordination
leases, not execution isolation or fencing. Direct OAuth sessions have no
refresh tokens and return to the App root after sign-in. A workspace operator
can administer the machine and data directly.
