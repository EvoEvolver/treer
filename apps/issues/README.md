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
| `ISSUES_REMINDERS` | `on` | `0`/`off`/`false`/`no` disables the hourly reminder poller |

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

Edits (including assignment/status) and comments notify creator and old/new
assignee, excluding the actor, through `/api/apps/SERVICE/messages` using the
**verified caller's** service token. Core therefore evaluates that caller's
Message Policy. Notification body contains only issue ID, link and one action
summary; titles, descriptions and comment bodies are excluded. Each event has
a stable Message idempotency key. Core disabled, denied or unavailable results
are recorded in the activity and API response; issue writes still succeed.
The browser displays failed/disabled/pending delivery in the timeline.

## Hourly reminders

On every full hour (epoch hour) the App collects issues that are not `done` or
`closed` and whose assignee is an Agent (stable ID `ag_…`; humans are never
messaged). For each such Agent it runs `treer agent show ID` and, only when the
Agent's `status` is `idle` and it is not busy, sends **one** Message listing up
to ten issue IDs (`+N more` beyond that) plus the App link with
`treer message send` under the App's own runtime identity. Bodies never contain
titles, descriptions or comments. No token is read, logged or put on a command
line; the CLI resolves identity from the inherited environment.

Anti-spam: at most one Message per Agent per hour. A `(hour, Agent)` row in the
SQLite `reminders` table survives restarts, and the Message idempotency key
`issues-reminder-AGENT-HOUR` makes a crash between send and record harmless. A
busy, unknown or unreachable Agent is skipped until the next hour; a failed send
is not recorded, so a restart within five minutes after the hour retries it. A
restart later in the hour does not catch up. Resolving, closing or reassigning
an issue stops reminders at the next hour. Set `ISSUES_REMINDERS=off` and restart
only this App to disable the poller. The App's Policy must allow `agent show`
and Messages to the assignee Agents; denials are skipped and logged by exit
status only.

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
work is the hourly reminder pass above, which has no retry queue beyond the
five-minute catch-up window.
The tree represents the currently loaded/filter-matching issues; load more to
include additional branches. Activity and child collections page separately.
Concurrent UI edits send an optimistic version; API callers should do likewise.
Create/comment are not idempotent: inspect after an uncertain response before
retrying. Notifications are best effort after commit, without a background
outbox or retry; an interrupted send remains pending in activity. Issue state
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
