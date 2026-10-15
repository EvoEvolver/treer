# Issues App

Issues is an ordinary workspace App for humans and Agents sharing a tracker.
It owns issues, Markdown source descriptions, priorities, labels, assignments,
parent/child hierarchies, blocked-by dependencies, comments, activity and leased
claims. It does not implement Core Issue/Task/Run/Artifact objects, scheduling
or Agent prompting. No Treer core code is changed.

## Design

The backend is Python 3.10+ standard library and SQLite; it does not need Node
at runtime. The browser UI is React (TypeScript, `ui/src`) built with a pinned
esbuild into four committed files in `dist/` that the App serves as-is, so
deploying is still `python3 apps/issues/issues.py` from a checkout. Treer owns the
identity directory, service tokens and browser OAuth; there are no local users.
All state, including OAuth session mappings, lives in one SQLite database.
Writes use `BEGIN IMMEDIATE`: permissions and claim ownership are checked inside
the same transaction as the mutation. Foreign references and parent/dependency
cycles are validated before commit. Progress counts all descendants, with
`done` and `closed` counted as completed. A lease is separate from assignment
and status; expiry immediately makes it available without resetting either.

The HTTP root negotiates an [Agent manual](AGENT.md) or the human UI. APIs use
`/v1/`, bounded pages and JSON errors. Browser assets (`./app.js`, `./app.css`,
`./boot.js`) and API calls are document-relative and preserve a Treer tunnel
prefix. Descriptions/comments are **text**, including their Markdown source;
no user content is inserted through HTML or evaluated, and the CSP allows only
same-origin scripts and styles.

### Interface

A quiet, dense, keyboard-first layout in the launcher's green palette, light and
dark (it follows the system theme until the toggle is used). Issues are grouped
by status (done/closed collapsed by default) or shown as a hierarchy tree, with
status, assignee, label and text filters; the detail pane edits status,
assignee and priority in place. On phones the list is one column and the detail
is a full-screen sheet with a Back button.

**Close** is one click in every list row, child row and the detail header
(reopen for closed issues); closing from a row or with `X` offers an Undo toast.
The buttons mirror the backend rule: they are disabled, with the reason shown,
unless the viewer is the creator, assignee, live lease holder or a human
owner/admin (a live lease held by someone else blocks everyone else). The server
still enforces permissions; the UI only explains them.

The **Needs me** view lists issues assigned to you that are not done/closed
(the API form is `GET /v1/issues?assignee=me&unresolved=1`; add `&label=x` or the
Label filter to narrow it).

Shortcuts: `J`/`K` move, `Enter`/`O` open, `X` close, `E` edit, `C` new issue,
`/` search, `Esc` back, `?` help.

### Building the UI

Sources live in `ui/`; `dist/` is generated and committed, like the launcher UI,
and is compared byte for byte with a fresh build. Versions are pinned exactly in
`package.json`/`package-lock.json`. After changing anything under `ui/`:

```sh
cd apps/issues
npm ci --ignore-scripts
npm run build:ui     # rewrites dist/
npm run check        # tsc, rebuild-and-compare, size budget, no absolute paths or HTML sinks
```

`.gitattributes` forces LF for `ui/` and `dist/`. Markdown rendering is
deliberately absent; adding marked/DOMPurify would need a security review.

## Deploy as a Managed App

Check for an existing name first. Do not use `--public`:

```sh
treer app list
treer app create --machine self --name issues --cwd treer --port 9440 \
  env -- \
  ISSUES_PROXY_URL=https://proxy.example.com/ \
  ISSUES_DB=/absolute/durable/path/issues.sqlite3 \
  python3 apps/issues/issues.py
treer app show issues
curl -fsS http://issues.treer/health
curl -fsS http://issues.treer/v1/me
```

Replace the Proxy URL with the trusted deployment's actual public Proxy URL,
not the App ingress or Controller loopback URL. Read the generated `public_url`
from the App record and open it in a browser, or select Issues in Treer's
workspace UI. With a Proxy containing human request identity forwarding, an
already logged-in user is recognized automatically on both paths; no second
App sign-in is required. The OAuth button appears only as a no-token fallback
for direct access or older Proxies. The App lazily discovers its own service/workspace and
public URL using `treer app list` and its injected runtime identity. This
requires the App's Policy to allow App discovery. It does not register a service
on behalf of the creating Agent. Discovery failure returns 503, never anonymous
access. Keep the command directory and Python installation available for recovery.
The runtime's own inherited HTTP proxy supports App discovery and `.treer`
clients. No Host/Controller restart is needed.

Embedded automatic identity works without wildcard ingress: discovery derives
the canonical Proxy `/api/workspaces/WORKSPACE/apps/APP_ID/ui/` root when no
public origin exists. Wildcard workspace ingress is still needed for the OAuth
fallback callback. For externally managed servers or a Policy that denies App listing, configure all identity values
explicitly after registering the service. Never select a service audience from
request headers or bodies.

## Deploy from a version directory (production layout)

The production App runs through `scripts/app-deploy` instead of a source checkout:
a stable entry executes `releases/<FULL SHA>` (selected by `current`), the database
lives in `ROOT/state/app.sqlite3`, and later releases are staged, health-checked and
rolled back by the tool (see the operator guide, "Deploy new Apps from version
directories"). Issues supports it with two additions:

- `GET /deployment-health` (unauthenticated, `no-store`, GET/HEAD only):
  `{"version": APP_DEPLOY_VERSION, "app_id": TREER_AGENT_ID, "ready": …, "database_ready": …}`;
  503 when the database does not answer. It exposes only a commit SHA and the App ID,
  and the probe result is cached for two seconds.
- `--proxy-url URL` overrides `ISSUES_PROXY_URL`. The deployment's saved command
  cannot carry environment variables, so the Proxy URL travels in the fixed arguments
  written by `init` (`… --db-env ISSUES_DB -- --proxy-url https://proxy.example.com/`).
  The entry sets `ISSUES_DB` and `APP_DEPLOY_VERSION` itself.

Migrating an existing App (rehearsed end to end by `tests/rehearse_migration.py`,
which prints every command and its rollback point; run it before touching a live App):

1. Back up the database with the online backup API (see *Backup and recovery*) and
   record `treer app show APP` in a private snapshot.
2. `app-deploy init`, `stage` (the component's tests run in the exported tree) and
   `bootstrap` — no downtime; deleting `ROOT` undoes it.
3. `treer app stop APP`; copy the old database into `ROOT/state/app.sqlite3` with the
   backup API and compare `integrity_check` and row counts; `treer app update APP --cwd
   <ROOT relative to the machine root> --command <the interpreter init printed> -- ROOT/entry.py
   run --root ROOT`; `treer app start APP`; check `/deployment-health` on loopback **and**
   through the Treer route, the unchanged App/service IDs and `/v1/issues`.
4. Rollback (the old database file is never modified): stop, `treer app update` back
   to the saved legacy command/cwd/args, start. Writes made on the new database after
   the cutover do not return to the old one.
5. Later upgrades: `app-deploy stage …` then `app-deploy deploy …`; a failing release
   is rolled back automatically together with the pre-deploy SQLite backup.

Use the App's stable ID or tree path (`/infra/issues`), not a bare name, with `treer app`.
`init` records the interpreter path exactly as given (since #51), so pass a stable shim such
as `/Users/dooms/.local/bin/python3.12`, which keeps following interpreter updates; the
interpreter is outside the release hashes, so keep the shim and its targets writable only by
trusted operators. App-deploy needs Python 3.11 or newer. A root created before #51 holds a
resolved, version-numbered path (for example `cpython-3.12.15`) in both `command` and
`launcher_command` of its `config.json`, which breaks when that interpreter is upgraded or
removed. Switch such a root explicitly: write the stable path into both fields of
`ROOT/config.json` (atomic replace, mode 0600, keep a copy), then
`treer app update /infra/issues --command <stable path>` (arguments and cwd are preserved and
the process restarts), and check `/deployment-health`, the unchanged App ID and that
`app-deploy`'s identity check still passes. The saved command and `launcher_command` must
stay identical or `deploy` refuses with `app_not_using_stable_entry`. Production was switched
this way after #51.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `ISSUES_PROXY_URL` | required | Trusted Proxy origin for verification, directory, OAuth and messages |
| `ISSUES_LISTEN` | `127.0.0.1:9440` | Private listener host:port, matching App declared port |
| `ISSUES_DB` | `.treer/apps/issues/issues.sqlite3` | Durable SQLite file, relative to runtime cwd if not absolute |
| `ISSUES_SERVICE_ID` | own Managed App record | Registered service audience; configure together with public URL |
| `ISSUES_PUBLIC_URL` | own Managed App record | Workspace-authenticated App origin, including trailing slash |
| `ISSUES_WORKSPACE_ID` | injected workspace or own record | Workspace boundary; required for external supervision |
| `TREER_BIN` | `treer` | CLI used only for own Managed App identity discovery |
| `ISSUES_URL` | `http://issues.treer/` | Client-side root override for the `issues` CLI |
| `ISSUES_NOTIFY_HOURLY_LIMIT` | `20` | Immediate Messages per recipient per hour before updates fold into one digest |
| `ISSUES_NOTIFY` | `on` | `0`/`off`/`false`/`no` disables issue-change notifications and digests |
| `ISSUES_REMINDER_SKIP_LABELS` | `epic` | Comma-separated labels (case-insensitive) whose issues are never counted by reminders; empty = none |
| `ISSUES_REMINDERS` | `on` | `0`/`off`/`false`/`no` disables the reminder poller |
| `ISSUES_REMINDER_FIRST` | `10m` | First reminder this long after an issue starts needing one (1m–24h) |
| `ISSUES_REMINDER_FACTOR` | `2` | Each later interval = previous × factor (1–10) |
| `ISSUES_REMINDER_MAX` | `24h` | Interval cap (≥ FIRST, ≤ 30d) |

Protect the database directory: sessions contain short-lived human access
tokens. Startup uses restrictive umask and database mode `0600`. OAuth uses
S256 PKCE, one-use state, hashed local session IDs, HttpOnly/SameSite cookies
and Secure cookies on HTTPS. Identity is reverified on every API call. The UI
never receives tokens. Authentication checks injected `X-Treer-Identity-Token`
first (even if a local cookie exists), then explicit Bearer, then an OAuth cookie.
Invalid/expired/other-audience supplied tokens fail without cookie fallback.
Forwarded tokens are verified on each request and are not saved as sessions.
Human writes, including injected human tokens, require JSON and
`X-Issues-Request: 1`. `Origin`, when present, must exactly match the configured
App or Proxy origin; `Sec-Fetch-Site`, when present, must be same-origin or
same-site. Null/untrusted origins and cross-site/none Fetch Metadata fail.
Non-browser clients may lack these headers, but human writes still require the
custom header. No cross-origin API access is enabled. CSP and nosniff protect assets.

## Permissions and notifications

Every authenticated principal in the workspace may read, create and comment.
Only creator, assignee, live lease holder, or human owner/admin may edit. A live
lease overrides editing privilege: only its holder may mutate, although anyone
may comment. Only unassigned issues or issues assigned to the caller can be
claimed. Lease duration is 1–86400 seconds; renew/release require a live owned
lease. Dependency resolution is required before claiming, starting or marking
done. Human administrators can edit after a lease expires, but cannot steal it.
This is a shared workspace tracker, not a per-issue ACL system. Treer's network
Policy and App permissions remain additional upstream gates.

Edits and comments notify the people they concern, never the actor, through
`/api/apps/SERVICE/messages` using the **verified caller's** service token, so
Core evaluates that caller's Message Policy:

| Event | Who is notified |
| --- | --- |
| Issue created | assignee |
| Comment | assignee |
| Status or assignee change | assignee, creator, and the previous assignee on reassignment |
| Any other edit (priority, labels, text…) | assignee |
| `@name` in a comment, or newly added to a title/description | the mentioned Agent, in addition to the above |

Claims/leases and updates that change nothing send nothing. A mention is
`@name` (must match exactly one Agent name, case-insensitive), `@/tree/path` or
`@ag_id`. Paths are resolved by Core's `recipients/resolve` with the **author's
own token** before the write (the Issues directory carries no paths), so the
author's Policy decides what a path means; the result must also be an Agent in
the author's directory. At most 10 paths are resolved per event, and none when
notifications are off. An ambiguous or unknown mention is ignored and reported:
`notification.unresolved_mentions` lists it and, for a duplicated name,
`notification.mention_candidates` offers the exact spellings: stable IDs, or a tree
path when the author is allowed to see it. Paths come from `treer agent ls -R /`
under the App identity (cached for a minute) but each one is shown only after
Core resolves it to the same Agent with the **author's own token** (at most 10
verification calls per event); a path the author's Policy hides is replaced by
the stable ID, so the App identity never discloses tree paths. The UI toast shows
them. Only Agents can be mentioned; the creator therefore hears plain comments
only when mentioned (`@author`).

A Message is one short fixed sentence plus a link, for example
`Issue #22 · Status open→done · by security-claude`; it never contains titles,
descriptions or comment bodies. There is exactly one notification path, so one
event produces at most one Message. Each event has a stable idempotency key
(`issues-event-ID`). Failed, disabled or unavailable delivery is recorded in the
activity and API response; issue writes still succeed. The browser displays
failed/disabled/pending delivery in the timeline.

**Anti-spam window.** Per (recipient, issue, **actor**) the first event is delivered
immediately with that actor's own token and opens a five-minute window. Later
events *by the same actor* in the window are only recorded
(`notification: coalesced`); when the window ends the App sends **one** digest
(`Issue #N · 3 more updates in the last 5 minutes: …`; at most the last 50
events are kept and the count reflects that) with `treer message send` under the
App's own identity and starts a new quiet window. Windows are keyed by actor on
purpose: the digest has no caller token, so it may only carry events of an actor
whose own token Core already allowed to message that recipient. An event by a
different actor never joins someone else's window; it is sent with its own token
(and refused by Core if that actor's Policy forbids it). A failed immediate send
discards its window together with anything merged into it meanwhile, so nothing
Core did not authorise is ever delivered later. A failed digest is retried at most
three times and then dropped. The App's Policy must allow the digest Message.

**Per-recipient cap.** Each recipient also has an hourly budget of immediate
Messages (`ISSUES_NOTIFY_HOURLY_LIMIT`, default 20, 1–1000). Beyond it, updates
from any number of issues are folded into one per-recipient digest per window
(`Issues · N more updates (over the hourly limit)…`), so the total sent to one
Agent is bounded whatever anyone @-mentions. A single comment @-mentions at most
10 Agents. The overflow digest is sent under the App identity too, so only an
actor whom Core already let message that recipient *in the current hour* (a
recorded successful send) may overflow into it; once the budget is spent, anyone
else's event is dropped and the activity shows `notification: throttled`. A
determined authorised sender can still use the 20 immediate slots, which delays
other people's notifications to the next digest (at most five minutes, nothing
is lost); there is no separate per-actor limit.

**Names in Messages.** Directory names are attacker-influenced text, so a name
appears in a Message only if it is 1–24 characters of `A–Z a–z 0–9 space . _ / -`;
otherwise the stable ID is shown instead. Titles, descriptions and comment bodies
never appear.

**Unresolved mentions.** An `@name` that matches no Agent or several is ignored,
and the write response and activity say so (`notification.unresolved_mentions`;
the UI toast lists them), so an author does not assume the Agent was reached.

`ISSUES_NOTIFY=off` disables all issue-change notifications (activity shows
`notification: off`); it is independent of `ISSUES_REMINDERS`. Pending digests
live in the SQLite `notify_windows`/`notify_budget` tables and survive restarts.

## Reminders (exponential backoff)

> **On by default.** Set `ISSUES_REMINDERS=off` (and restart only this App) to disable
> the poller. Anyone who may edit an issue can set its assignee, so being able to assign
> an issue to an Agent also lets that person make the App message that Agent. The
> Message text is fixed (issue IDs and a link only), is sent as the App's own identity,
> and remains subject to Treer's Message authorization and Policy.

An issue **needs a reminder** while it is not `done`/`closed`, its assignee is an Agent
(stable ID `ag_…`; humans are never messaged) and it carries none of the labels in
`ISSUES_REMINDER_SKIP_LABELS` (default `epic`). For each such (assignee, issue) the App keeps
a backoff in SQLite (`reminder_backoff`, so restarts keep it): the first reminder is due
`ISSUES_REMINDER_FIRST` (default `10m`) after the issue started needing one, and every
later interval is the previous one times `ISSUES_REMINDER_FACTOR` (default `2`) capped at
`ISSUES_REMINDER_MAX` (default `24h`): **10m, 20m, 40m, 1h20m, 2h40m, 5h20m, 10h40m,
21h20m, then every 24h**.

- **Reset.** Any real activity on the issue (a comment, status or assignee change, an
  edit, a lease) restarts the sequence at the first interval, counted from when the
  activity happened; a new assignee starts from scratch and the old state is dropped. An
  issue that stops needing reminders (resolved, labelled `epic`, unassigned, assigned to a
  human) loses its state and starts fresh if it returns. Note that comments reset the
  clock: they count as activity, including comments by other people.
- **Only when idle.** At the due time the App runs `treer agent show ID`; only when the
  Agent's `status` is `idle` and it is not busy does it send. A busy, unknown or
  unreachable Agent gets nothing and its interval does **not** grow; the next poll looks
  again. Polling runs every minute (or at the nearest due time) and only Agents that
  have something due are probed.
- **One Message per Agent per moment.** Everything due for the same Agent at the same poll
  is one Message listing up to ten issue IDs (`+N more` beyond that) plus the App link
  (only a plain `http(s)` URL is ever included) sent with `treer message send` under the
  App's own runtime identity; each issue keeps its own backoff. Bodies never contain titles,
  descriptions or comments. No token is read, logged or put on a command line.
- **Idempotent and honest about failure.** The Message idempotency key covers the Agent,
  the issue set and each issue's backoff step, so a crash between send and record cannot
  duplicate a reminder and different steps never collide. A failed send is not recorded;
  the next poll retries with the same key.
- **Deployment.** Issues unresolved when this version first runs simply start their first
  interval then; nobody receives a burst. The old hourly `reminders` table is no longer
  written (it is left in place and harmless).
- **Configuration is strict.** `ISSUES_REMINDER_FIRST` 1m–24h, `ISSUES_REMINDER_FACTOR`
  1–10 and `ISSUES_REMINDER_MAX` at least FIRST and at most 30d, written as `90s`, `10m`,
  `1.5h`, `24h`. An invalid value stops the App at start-up with the variable named; it is
  never silently replaced by a default.

The App's Policy must allow `agent show` and Messages to the assignee Agents; denials are
skipped and logged by exit status only.

## Agent client

Run the executable `apps/issues/issues` (or download `/issues`, inspect, then
install under a user-owned bin directory). Python urllib honors the injected
HTTP proxy, so Agents use `http://issues.treer` without manually supplying
identity. `list/show/create/comment/assign/status/claim/children` output JSON;
errors output JSON and exit nonzero. `list`, `show`, `children` support paging.
See the [runtime manual](AGENT.md) for commands and the complete API contract.

## Backup and recovery

Use SQLite's online backup API, which includes committed WAL contents in a
consistent single-file snapshot:

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

Do not copy only a live `.sqlite3` file while WAL writes are active. For an
offline restore stop **only this App** with `treer app stop issues`, preserve the
old database and any `-wal`/`-shm` sidecars, restore the backup at `ISSUES_DB`,
remove obsolete sidecars from the restored location and start this App. Backups
include OAuth sessions and sensitive issue text: protect access and retention.
Expired tokens cannot authenticate after restore because identity is reverified.
Do not run multiple App versions against the same file; schema migration is not
yet versioned.

## Verification

```sh
python3 -m unittest discover -s apps/issues/tests -p 'test_*.py' -v
(cd apps/issues && npm ci --ignore-scripts && npm run check)
cd apps/issues/tests
npm install --ignore-scripts
npx playwright install chromium
npm test
# From repository root:
node scripts/check-docs.mjs
```

Playwright is a test-only dependency, separate from the UI build in `apps/issues`. Browser tests can
reuse an installed module through `ISSUES_PLAYWRIGHT_MODULE=/absolute/module`.
Screenshots go to `/tmp/treer-issues-screenshots` (override
`ISSUES_SCREENSHOT_DIR`). Tests use the real Issues server and a local Treer
contract/gateway double. Four browser tests cover already logged-in humans on
Workspace ingress and inside a distinct-origin control-plane iframe at the
canonical `/api/workspaces/ws_test/apps/app_issues/ui/` prefix, each at desktop
and 390px. The gateway issues/validates fresh 60-second human tokens, strips
spoofed identity headers/platform cookies and rewrites upstream Host. Tests
assert no OAuth requests or App session cookies, then create/assign/status/
comment/claim/renew/release, check hierarchy, assets/API prefixes, XSS and both
themes. Two separate tests retain direct OAuth fallback coverage. Rust Proxy
tests exercise actual middleware, signing, sanitizers and controller HTTP
streams against randomized schemas in the isolated local PostgreSQL test DB;
the browser gateway double is not proof of deployment acceptance. API tests cover identity spoofing, valid/expired/other-audience human tokens,
identity-header precedence, Origin/Fetch Metadata rejection, workspace/audience mismatches, permissions, lease concurrency/expiry/renew/release,
hierarchy/dependency cycles, rollups, notification degradation and every CLI
command. Fixtures, browser processes and client subprocesses clear inherited
`TREER_*`, `AGENT_UI_*`, `CODEX_UI_*` and proxy settings. The initial App-only implementation omitted Core database tests; human browser
identity forwarding is a Proxy auth change and additionally runs
`cargo test -p treer-proxy` with
`TREER_TEST_DATABASE_URL=postgres://treer:treer@127.0.0.1:55432/treer_test`,
`cargo test -p treer-protocol`, `cargo fmt --all --check` and
`cargo clippy -p treer-proxy -p treer-protocol --all-targets -- -D warnings`.
Each database test uses a random schema; do not select a shared database.

## Known limits

This App targets small trusted teams. Graph operations load workspace issue
metadata into memory; no full-text search, pagination snapshot, attachments,
email, kanban, deletion or per-issue privacy is provided. The only background
work is the reminder poller and the five-minute digest worker described
above; neither has a retry queue beyond the limits stated there.
The tree represents the currently loaded/filter-matching issues; load more to
include additional branches. Activity and child collections page separately.
Concurrent UI edits send an optimistic version; API callers should do likewise.
Create/comment are not idempotent: inspect after an uncertain response before
retrying. Immediate notifications are best effort after commit, without an outbox or
retry (the next event retries); an interrupted send remains pending in activity. Issue state
is canonical only in this App's SQLite file. Claims are coordination leases,
not execution isolation, fencing tokens, status resets or automatic Agent runs.
A workspace operator can administer the machine and data directly.

Automatic human identification requires deploying the new Proxy **and** loading
the current Issues runtime code; older Proxies only authorize passage and still
need OAuth. This change is tested locally but has not deployed or restarted
Proxy, Host, Controller or Apps. Live Treer acceptance follows the leader's
release/deployment. Other Apps that only read their private OAuth cookie (for
example the current Mail implementation) must adopt the standard identity-header
order separately. Direct/legacy OAuth uses the configured public callback root,
so it does not preserve an original embedded-page return URL. No OAuth refresh
tokens exist; direct sessions require reauthentication after expiry. The local
browser gateway reproduces the two forwarding contracts using an HTTP test
cookie and signed HMAC test tokens; it is not the production Ed25519 issuer or
the full React control plane. Production signing, TTL, audience and membership
revocation are checked by the Rust Proxy tests, and real release acceptance is
still required.

## Security review notes

OAuth callbacks now require the initiating browser's expiring HttpOnly nonce
as well as state/PKCE; previously pending OAuth flows must restart after upgrade.
Authenticated upstream redirects are refused. Startup tightens the dedicated
state directory to 0700 and uses private SQLite files/sidecars. Do not point
state at a shared directory. Sessions and issue content remain plaintext;
restrict/encrypt backups and disks. Same-UID Managed Apps are not isolated;
use a separate unprivileged user or container/VM for critical services.
Input limits and claim leases do not provide global rate/concurrency isolation.

## Interface and assignee directory

The interface uses a compact sidebar, status-grouped issue rows, local search,
list/detail panes, quick personal/status views, and light/dark themes. `C` opens
a new issue and `/` focuses search outside form fields. At 390px the panes stack.
This is an initial Linear-inspired interface, not feature parity: projects,
cycles, saved views, drag-and-drop and a kanban board are not implemented.

Assignee pickers in creation, editing, details and filtering share one directory:
humans appear under People; Agents appear in expandable folders using the
canonical `path` supplied by Treer's permission-filtered App identity directory.
The selected assignee keeps its full path visible; activity and list tooltips also
include paths. Missing paths display the stable Agent ID explicitly, rather than
leaving identically named Agents indistinguishable. Search matches names, paths
and IDs, including identically named Agents in different
folders. Selection always sends the stable principal ID, never a display name
or a folder path. The App performs no additional privileged tree lookup. Older
Proxies without directory paths show Agents under Placement unavailable until
the Proxy is upgraded. Only identities visible to this App are listed.

Browser tests additionally check nested folders, collapse/expand, human choices,
name/path search, duplicate names, legacy missing paths, Escape handling and
390px picker overflow. Screenshots are test artifacts under `/tmp`, not committed.
