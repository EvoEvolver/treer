# Issues App

A workspace issue tracker for humans and Agents: issues with Markdown
descriptions, comments, priorities, labels, assignees, parent/child hierarchy (a
DAG), blocked-by dependencies, `#N` references, full-text search and `note`
entries. Assigning an Agent or @mentioning it sends a Core Message and wakes it.

The HTTP root negotiates the [Agent manual](AGENT.md) (CLI and `/v1` API) or the
human UI. The UI uses only that same API, so everything a person can do an
Agent can do. This README covers design and operation.

## Design

- Python 3.10+ standard library and SQLite, one database file; writes use
  `BEGIN IMMEDIATE`. No Node at runtime: the React UI in `ui/src` is built into
  the committed `dist/` (`npm ci --ignore-scripts && npm run build:ui`;
  `npm run check` verifies the bytes, size budget and absence of HTML sinks).
- Treer owns identity: an injected `X-Treer-Identity-Token`, a Bearer token or an
  OAuth session (S256 PKCE fallback for direct browser access) is verified on
  every request against the App audience and the workspace directory. Human
  writes need JSON, `X-Issues-Request: 1` and a same-site Origin.
- **Permissions:** any verified workspace principal may create, edit, comment,
  link, hide and restore anything. Activity records who changed what, so history
  is the safeguard. Treer Policy remains the upstream gate.
- **Hide, never delete:** `DELETE /v1/issues/N` and
  `DELETE /v1/issues/N/activity/E` (comments only) set a restorable `hidden`
  marker; hidden issues are listed only with `hidden=1|only`, hidden comments
  appear as stubs unless `hidden=1`, and neither is searched.
- **Markdown** is rendered by a small converter into React elements (no HTML
  is ever injected; links only for http(s)). The CSP allows same-origin scripts
  and styles only.
- **UI:** All / Mine / Open, status, assignee, label, hidden and text filters,
  list or hierarchy tree; comments newest first, collapsed to their title or
  first line; system events and hidden text behind toggles. Shortcuts: `J`/`K`,
  `Enter`, `X` close, `E` edit, `C` new, `/` search, `?` help.

## Notifications and wake-ups

A create, edit or comment that concerns someone sends one Core Message per
recipient kind with the **actor's** token (Core applies the actor's Message
Policy). Recipients: the assignee (every event), the creator (status/assignee
changes), the previous assignee (reassignment) and Agents newly @mentioned. The
actor is never notified. Messages carry a fixed sentence, the sanitized comment
title and `issues show N --titles` (Agents) or the issue link (people), never
issue text.

A Message does not wake an idle Agent. So for an Agent that was **just assigned
or @mentioned**, and whose Message Core accepted, the App also runs
`treer agent prompt AGENT "Issues #N: assigned to you by NAME. Message msg_…. Read it: issues show N --titles (or LINK)"`
under its **own** identity. Only Agents the actor could message are woken, and
the text contains no author-written content. At most one wake per (Agent,
issue) per `ISSUES_WAKE_MINUTES`; a transient failure frees the slot. The result
(`prompted`, `deduped`, `denied`, `failed:<code>`) is stored in the activity
entry's `notification.wake`. Humans are never prompted.

**Grant `agent.prompt` to the App.** Prompting is denied by default; without the
grant wake-ups are recorded as `denied` and Messages still go out. Add a rule to
the workspace Policy (see [Policy](../policy/README.md)), using the App's
runtime ID from `treer app show /issues`:

```json
{"id":"issues-app-wakes-agents","priority":40,"effect":"allow",
 "subjects":[{"id":"app_RUNTIME_ID"}],"actions":["agent.prompt"],
 "resources":[{"kind":"agent"}]}
```

Narrow `resources` with a `tree` selector to limit which Agents it may wake.

## Deploy

As a Managed App (never `--public`):

```sh
treer app create --machine self --name issues --cwd treer --port 9440 env -- \
  ISSUES_PROXY_URL=https://proxy.example.com/ ISSUES_DB=/durable/issues.sqlite3 \
  python3 apps/issues/issues.py
```

Or with `scripts/app-deploy` version directories (database at
`ROOT/state/app.sqlite3`; pass `--proxy-url` as a fixed argument; health at
`GET /deployment-health`). The App discovers its service, workspace and public
URL with `treer app list`; for external supervision set `ISSUES_SERVICE_ID`,
`ISSUES_PUBLIC_URL` and `ISSUES_WORKSPACE_ID`.

| Variable | Default | Meaning |
| --- | --- | --- |
| `ISSUES_PROXY_URL` / `--proxy-url` | required | Trusted Proxy URL for identity, directory, OAuth and Messages |
| `ISSUES_LISTEN` | `127.0.0.1:9440` | Listener; match the App's `--port` |
| `ISSUES_DB` | `.treer/apps/issues/issues.sqlite3` | SQLite file |
| `ISSUES_NOTIFY` | `on` | `off` disables Messages and wake-ups |
| `ISSUES_WAKE` | `on` | `off` keeps Messages but never prompts |
| `ISSUES_WAKE_MINUTES` | `10` | Wake dedupe window per (Agent, issue), 0–1440 |
| `TREER_BIN` | `treer` | CLI used for discovery and `agent prompt` |
| `ISSUES_URL` | `http://issues.treer/` | Root used by the `issues` client |

`--leader-agent`/`--release-agent` from earlier releases are accepted and
ignored, so existing deployments start unchanged.

## Upgrading older databases

The App starts on a database written by earlier releases without migrating or
deleting anything. Tables of retired features (reminders, notification
windows/policies/budgets, shipped signals, stall tracking) and issue fields
(`claim`, `waiting_on`, reminder clocks) are left as they are and ignored;
the only schema change is a new `wakes` table. `tests/test_migration.py` checks
this against `tests/fixtures/legacy-issues.db`, written by the previous code
(`tests/fixtures/make_legacy_db.py`).

Back up with SQLite's online backup (never copy a live file alone):

```sh
python3 - /durable/issues.sqlite3 /secure/backup.sqlite3 <<'PY'
import sqlite3, sys, os
os.umask(0o077)
source = sqlite3.connect('file:' + sys.argv[1] + '?mode=ro', uri=True)
target = sqlite3.connect(sys.argv[2]); source.backup(target); target.close(); source.close()
PY
```

Backups contain OAuth sessions and issue text; protect them.

## Verification

```sh
python3 -m unittest discover -s apps/issues/tests -p 'test_*.py'
(cd apps/issues && npm ci --ignore-scripts && npm run check)
(cd apps/issues/tests && npm install --ignore-scripts && npx playwright install chromium && npm test)
```

Browser tests run the real server behind a local gateway double (Workspace
ingress, the embedded `/api/workspaces/…/ui/` prefix and the OAuth fallback) at
desktop and phone widths; Agent prompts are recorded by the fixture.

## Limits

Built for small trusted teams; graph operations load issue metadata into memory.
No attachments, per-issue privacy or hard deletion. Create and comment are not
idempotent. Messages and wake-ups are best effort after commit (no outbox).
