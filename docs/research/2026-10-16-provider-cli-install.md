# Provider CLI installation contract (Issues #123)

Part 1 implements missing outer executable errors and conservative reservation
compensation. The dedicated installation routes below are an approved design
(security comment 1928), pending implementation; they are not available endpoints.
Leader chose bounded results plus polling, not real-time output (comment 1926).

Base: `/api/workspaces/{ws}/machines/{machine}/provider-clis`.
Proxy and Controller advertise `machine.provider-cli-install.v1`; an old peer
fails explicitly with 501 and the Console hides unsupported installation UI.

`GET /recipes` requires current human workspace access and Policy `machine.exec`
on that machine. The Controller serves a bounded, versioned launcher recipe table
embedded in the official Controller release, not a user-installed directory,
App, browser-supplied argv or remote recipe URL. Core validates schema and hashes.
Hash rules will be fixed against launcher-dev's serialized table before coding;
hashes below stand for SHA-256 of those exact versioned bytes.

```json
{
  "schema_version": 1,
  "machine_id": "srv_example",
  "platform": "windows-x86_64",
  "controller_source_sha": "<40 hex>",
  "table_hash": "<64 hex>",
  "recipes": [{
    "provider_id": "claude",
    "recipe_id": "claude-windows",
    "recipe_version": 1,
    "recipe_hash": "<64 hex>",
    "supported": true,
    "display": {"executable": "<vetted executable>", "args": ["<vetted argv>"]},
    "check": {"executable": "claude", "args": ["--version"]},
    "login_hint": "Log in manually from your own terminal."
  }]
}
```

Unsupported entries contain `supported:false` and a fixed `unsupported_reason`,
no executable recipe. Exact vetted IDs/binary names determine install buttons;
the UI does not inspect raw errors, paths, logs or PATH to guess providers.

`POST /prepare` takes `{recipe_id,recipe_version,recipe_hash}`; it returns
`{confirmation_token,expires_at,machine_id,table_hash,recipe}` with the exact
recipe displayed before explicit confirmation. Current human session and
`machine.exec` are required at both prepare and install. App service tokens
cannot stand in for that session. POST uses existing Origin/CSRF protections.
Tokens are random 256-bit, expire after 120 seconds by database time, and only
their hashes are stored. Bind workspace, machine, user, session, recipe
ID/version/hash, table hash and Controller source SHA. Never put tokens in
URLs, audit, logs or browser persistent storage.

```json
{"recipe_id":"claude-windows","recipe_version":1,"recipe_hash":"<64 hex>"}
```

```json
{
  "confirmation_token":"<opaque single-use token>",
  "expires_at":"2026-10-16T17:02:00Z",
  "machine_id":"srv_example","table_hash":"<64 hex>",
  "recipe":{
    "provider_id":"claude","recipe_id":"claude-windows","recipe_version":1,
    "recipe_hash":"<64 hex>","supported":true,
    "display":{"executable":"<vetted executable>","args":["<vetted argv>"]},
    "check":{"executable":"claude","args":["--version"]},
    "login_hint":"Log in manually from your own terminal."
  }
}
```

`POST /install` takes only `{confirmation_token}`. Proxy atomically consumes it
in PostgreSQL and inserts a unique operation before dispatch. Policy and the
current Controller table are rechecked; Controller also checks recipe hashes.
Changed table/Controller, expired or consumed token returns 409, never executes
an alternative, and requires a fresh display/confirmation. A busy machine
returns 409 before consumption. No automatic retries. Response:

```json
{"operation_id":"<UUID>","machine_id":"srv_example","status":"queued","poll_after_ms":1000}
```

`POST /check` takes the recipe ID/version/hash, requires the same human/Policy
checks and runs only its vetted version probe, at most 10 seconds. It returns
`{installed,version,exit_code,timed_out,stdout,stderr,truncated}`. Unsupported or
changed recipe is explicit; login is never performed or credential-handled.
Version/output is bounded text, not HTML or ANSI interpreted by the UI.
A successful probe proves current binary availability, not the outcome of an
earlier unknown installation or the termination of its descendants.

```json
{
  "installed":true,"version":"<bounded version output>","exit_code":0,
  "timed_out":false,"stdout":"<bounded plain text>","stderr":"",
  "truncated":false
}
```

`GET /operations/{id}` checks the operation's creator, workspace and machine
plus current human membership and `machine.exec`; identifiers alone grant no
read access. Another session of the same creator may resume polling after
reauthentication. Poll once per second; terminal states stop polling.

```json
{
  "operation_id":"<UUID>","machine_id":"srv_example",
  "recipe_id":"claude-windows","status":"running",
  "elapsed_ms":1200,"exit_code":null,"timed_out":false,
  "stdout":"","stderr":"","truncated":false
}
```

States: `queued`, `running`, `succeeded` (exit 0), `failed` (known nonzero or
known pre-start failure), `timed_out` (deadline reached, cleanup confirmed),
`unknown` (execution/cleanup result cannot be confirmed). Unknown stays unknown
until an independent observation resolves it; it never authorizes resubmission.
Structured fixed `error_code` can accompany failure/unknown, never raw exceptions.
At completion, stdout/stderr tails are each at most 32 KiB, UTF-8 with replacement;
`truncated` reports discarded bytes. Nonterminal responses expose no output tail
in MVP. Closing the UI or losing the HTTP connection only stops polling, not
the job, and does not imply cancellation. No client-controlled command/cwd/env,
timeouts or output limits.

Dedicated jobs have a five-minute execution deadline and at most one active
operation per machine, enforced by a shared PostgreSQL unique constraint, not
a Proxy-local lock. Existing general exec keeps its 30-second limit. Job
output is never in audit/application logs or Proxy durable storage. Controller
results require private bounded storage, retained for 24 hours with a fixed
64-completed-operation cap; exhaustion refuses new installation rather than
silently deleting retained results. Expired output returns 410 explicitly.
Proxy retains only operation scope/status/recipe hashes/timestamps. A lost
dispatch/result remains ambiguous; reconnect must not replay installation.
Proxy/Controller loss transitions unfinished operations to `unknown` after a
bounded grace period. Before preparing another install after unknown, require
a fresh check; that check does not itself clear uncertain process ownership.
Prepare admits at most four live tokens per user/machine and ten requests per
minute per user/machine, enforced with database time and shared PostgreSQL state.
Expired tokens are removed by bounded cleanup. Operation reads are creator-only
in MVP (no implicit owner override); current membership and Policy are rechecked.
Before implementation, prove process ownership/descendant cleanup on each OS;
failure to establish it must fail before execution or produce unknown, never
claim cancellation. This is a review gate, not supplied by general exec today.

Audit prepare/accepted/finished with actor, machine, recipe ID/version/hash,
argv hash and exit code/status; no token, output, PATH or credential. Core does
not handle provider login. Installation executes third-party code as the
machine account under the user's explicit authority; it is not a sandbox.

Recipe validation is a prerequisite, not browser confirmation alone: executable
is a bare name from a fixed allowlist; argv is a string array executed directly,
never through a shell, sudo, su or elevation wrapper. Npm packages require exact
versions and an explicit pinned registry, never latest/ranges/tags. The table
owner reviews package scope/version and lifecycle scripts and uses ignore-scripts
where supported by the vendor. Download recipes, if introduced, require a pinned
URL/host and SHA-256 with separate download, verification and execution steps;
redirects cannot escape the pinned host. Unsupported entries cannot prepare.
Display and spawn use the same expansion function, checked item-for-item in tests.
