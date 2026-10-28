# Develop or onboard an App

Read [App guidelines](../../../apps/GUIDELINES.md) for the HTTP/auth contract,
[App overview](../../../apps/README.md) for runtime ownership and the
[operator guide](../../../docs/operator-guide.md#deploy-new-apps-from-version-directories-posix)
for version-directory deployment. An AIS is an Agent interface, not an App.

## Pick the lifecycle before wiring a URL

A Managed App is one long-running HTTP process with one declared UI port.
Treer creates its durable workload identity, service and workspace-authenticated
origin; its Controller starts/stops/recovers that process. Use this form for a
new App or an existing web program whose startup Treer should own.

An existing HTTP process can keep its external supervisor and be registered as
a private workspace **service**. This is not a Managed App and does not adopt
its process, give it an App tree node, or implement Treer authentication inside
it. There is no `treer app register` CLI command. Do not start a duplicate server
on an occupied port or delete/recreate an existing Managed App to update it.
For a tree App node, explicitly arrange an authorized supervisor handover to
`app create`, or implement a Managed App wrapper for the existing service.

Inspect before changing anything:

```sh
treer app list
treer agent ls -R /
treer app create --help
treer app update --help
treer service register --help
```

For an authorized, already-configured program (replace the machine, physical
machine-relative cwd and free port):

```sh
treer app create --machine target-machine --name reports --folder /team \
  --cwd apps-deployed/reports --port 8877 \
  /absolute/stable/python3 -- server.py --config /private/reports.json
treer app show /team/reports
```

Do not use `--public` by default. `--cwd` is relative to the enrolled machine
root, not the developer's current machine; executable/config paths are on the
target machine. Config **paths** can appear in argv; secrets cannot. For
production, substitute the stable command/args/cwd returned by app-deploy init
below, rather than launching this mutable source example.

To keep an external process under its existing supervisor instead:

```sh
treer service register --machine target-machine --name legacy-reports \
  --host 127.0.0.1 --port 8877 --protocol http
treer service list
```

Registration requires the caller's permissions and a reachable target; it is
not a health proof. Use the returned service record. Do not publish a raw legacy
listener to bypass its application's access controls. A wrapper's upstream must
be fixed by operator configuration, never a request-supplied arbitrary URL;
validate caller identity/ACLs and strip identity headers before any unrelated
upstream. Private ingress alone does not make a legacy App multi-tenant safe.

## Build the App boundary

- `/` negotiates HTML for browsers and Markdown instructions for Agents; JSON
  belongs under `/v1/...` or `/api/...`. Unknown API routes stay JSON errors/404,
  not a successful SPA root. Include a small read-only health endpoint.
- Browser UI uses React with pinned dependencies and committed generated assets
  (see Issues or Telegram); the server serves the built files without Node at
  runtime. Rebuild and compare artifacts in checks. Render untrusted content as
  text; retain strict CSP and avoid HTML sinks.
- Resolve assets and API URLs from the App's document/base URI, preserving
  `/api/workspaces/WORKSPACE/apps/APP_ID/ui/`. Do not hard-code `/app.js` or
  origin-root APIs. Test root-to-detail navigation at desktop and 390px through
  both embedded and dedicated-origin gateways.
- First verify supplied `X-Treer-Identity-Token` with Core
  `POST /.treer/apps/identity/verify` for **your own** service audience/workspace
  on every request. Check active principal/current membership and enforce your
  own ACL by stable ID. Never trust role/name/actor fields in request JSON or
  merely decode a JWT. Reject invalid/expired/wrong-audience headers even when
  a cookie is valid. Verify Agent Bearer/service tokens by the same boundary.
- Workspace Proxy supplies a fresh 60-second human identity, so signed-in users
  do not click another login button. Without a supplied identity, standard
  Treer App OAuth/PKCE may provide a direct/legacy fallback; do not invent local
  accounts or magic-link credentials. Keep tokens out of frontend storage/logs.
  Public ingress deliberately supplies no human identity.
- Browser mutations still need exact configured Origin/Fetch Metadata checks,
  non-GET JSON/custom-header requests and narrow CORS. Use a custom header
  outside `x-treer-*`: the Proxy strips client headers in that namespace.
  OAuth state needs an expiring, single-use browser nonce in addition to PKCE;
  validate return paths and refuse credential-bearing redirects.
- When acting for a caller, use that caller's verified authority to Core; an App
  must not silently substitute its own stronger identity. Background side
  effects need their own explicit authorization and visible failure handling.

Managed Agent HTTP clients honor their inherited proxy when calling
`http://NAME.treer/` or `http://SERVICE_ID.treer/`; it supplies the caller's
service-scoped token. Prefer a discovered stable service ID where names collide.
Use the literal `svc_...` ID unchanged: a public `svc-...` ingress hostname is
not the same name. Do not disable the proxy or hand-copy workload tokens.
Browser humans use the App record's generated `public_url` or Treer's embedded
page, not a guessed virtual hostname. Older Apps can differ: inspect their
README/auth implementation before claiming automatic identity support.

## Versioned deployment and rollback

Use app-deploy on POSIX, locally on the App's enrolled machine; it is not a
remote installer and does not support Windows. It needs Python 3.11+ and Git.
Choose private physical paths inside the enrolled root and a trusted absolute
interpreter. Review a full 40-character commit and test the complete exported
component, including sibling imports/assets.

For a **new** App with a single SQLite database honoring `REPORTS_DB`, start with:

```sh
python3 scripts/app-deploy init --root /machine/apps-deployed/reports \
  --machine-root /machine --command /absolute/stable/python3 \
  --entry server.py --db-env REPORTS_DB
python3 scripts/app-deploy stage --root /machine/apps-deployed/reports \
  --repo /source/treer --commit FULL_40_CHARACTER_SHA --component apps/reports \
  --check-json '["/absolute/stable/python3","-m","unittest","discover"]'
python3 scripts/app-deploy bootstrap --root /machine/apps-deployed/reports \
  --release FULL_40_CHARACTER_SHA
```

`apps/reports` and its test command are an example for your own App, not a
bundled directory. Capture init's exact launcher command, argv and relative cwd
and use those fields in `treer app create`. The App must read `REPORTS_DB`
(`ROOT/state/app.sqlite3`), keep all writes outside immutable release assets and
report no-store `/deployment-health` containing `version` from
`APP_DEPLOY_VERSION`, its stable runtime `app_id`, `ready`, `database_ready`.
Do not report ready until migrations and a real DB check pass.

After the first version is running, stage the next frozen commit, then:

```sh
python3 scripts/app-deploy deploy --root /machine/apps-deployed/reports \
  --release NEXT_FULL_SHA --app STABLE_APP_ID --treer /absolute/path/to/treer \
  --operation-id reports-upgrade-unique-id --timeout 30 \
  --health-url http://127.0.0.1:8877/deployment-health \
  --route-url http://SERVICE_ID.treer/deployment-health
```

Deploy verifies old identity/health, stops **only this App**, takes a SQLite
backup, changes current/previous, restarts the same App and verifies both URLs.
Failure restores that DB snapshot and old version. Use `recover --help` for an
interrupted recorded operation; replaying a completed operation only verifies
it, and is not a deliberate rollback command.

An existing mutable-checkout App needs an explicit first-adoption plan: preserve
startup and complete state/config, quiesce writers, copy data and migrate the
saved command with `app update`. There is no automatic old-version rollback
baseline until that App has a versioned, healthy entry. Startup-field rollback
alone is not code or data rollback. Successful later downgrades need matching
code/schema/config; do not discard later business data without authorization.

The tool backs up **one SQLite DB**, not secrets/config, attachments, other
writers, queues or external side effects. Back those up/quiesce separately;
use another deployment strategy if this is not a complete rollback boundary.
Keep receipts: exact SHA, identity, private backup paths, integrity/counts,
local/routed health, assets, rollback limits. Never put credentials in receipts.

## Verification and isolation

Test valid/forged/expired/other-audience tokens, changed membership, ACL denials,
OAuth/state/CSRF, text rendering, real forwarding behavior and relative URLs.
Strip inherited workload/harness credentials and proxy variables for local
fixtures; use fake Core/CLI, random ports, no real mail or machine mutations.
Fakes invoking Treer must use [the CLI contract](../../../apps/cli_contract.py)
and retain Controller semantic checks; matching a permissive fake is insufficient.

Keep dedicated state directories 0700 and data/config/backups 0600. Stores are
plaintext unless documented otherwise. Managed Apps run with their machine
account's OS authority: 0600 cannot isolate other Agents under the **same UID**.
Use a separate unprivileged OS account/container/VM, minimal mounts/network and
resource limits for critical services. Never expose Host/Docker sockets. Tree
paths are useful locators; Core stable IDs and policy decide authorization.
