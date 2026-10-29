# Agent Hub

A stdlib Python Managed App that runs **one task per fresh coding Agent**.
Long-lived Agents that do everything accumulate context; coding Agents work
best with a clean one. For every job the hub starts a new worker from a
versioned template, gives it only the task and its input handoffs, collects a
structured handoff, then archives the job and ends the worker so its memory is
released. Callers chain jobs themselves (`inputs.handoffs`); there are no
built-in pipelines and no approval steps.

Workers are **App-owned subtree Agents**: `/<hub>/jobs/<id>` in the hub's
[App tree](../../docs/app-tree.md), never Agents in the workspace Tree. Each is
a `treer-launch --no-register` child process of the hub; Core creates no Agent,
credential or Host process for it.

The caller manual is [AGENT.md](AGENT.md) (served as Markdown at `/`); the
browser UI ([ui.html](ui.html)) uses the same HTTP API.

## Model

| Concept | Stored as |
| --- | --- |
| Template | `name`, immutable `version`s of `agent_md`, `harness` (`codex`/`claude`), optional `model`/`effort`, `cwd` (`job`, `home` or a Host-relative path). Bundled ones in [templates/](templates/) are seeded at first start; a bundled template nobody edited (latest version by `seed`) follows the bundled file as a new version, an edited one is never overwritten. |
| Job | `j-0001`…: template@version, task, inputs (`handoffs`, `repo`, `branch`, `notes`), requester (verified Core principal), status, worker process (pid, loopback port, instance id, restart count), hash of the per-job token, provider session ID. |
| Handoff | Markdown with front matter (`job`, `template`, `status: done\|partial\|blocked`, `summary`, optional `branch`, `commits`) and sections *What changed*, *Verification*, *Open questions*, *Next steps*. Validated on submit. |
| Archive | the handoff, the worker's transcript export (`/v1/transcript`, with the session id), git refs from the handoff. |

## Worker processes

The hub spawns each worker itself:

```text
<launcher> --harness H --instructions STATE/instructions/NAME@VERSION.md [--model M] [--effort E]
           --no-register --state-dir STATE/workers/<id>/launcher
           --listen 127.0.0.1:0 --port-file STATE/workers/<id>/port [--session-id S]
```

- **Process:** a child of the hub in its own session and process group
  (`start_new_session`), stdout/stderr to `STATE/workers/<id>/launcher.log`,
  working directory the job directory (`--jobs-root`/`<id>` for `cwd: job`).
- **Environment:** the hub runtime's own environment, so `treer` in the worker
  acts as the hub (`TREER_AGENT_ID`, workload credential, Controller URL).
  `AGENT_UI_*`, `CODEX_UI_*`, `RUST_SPIKE_*`, `TREER_AGENT_PRIVATE_STATE_DIR`
  and `TREER_RUST_SPIKE_STATE_DIR` are removed; `--no-register` makes the
  launcher ignore the inherited identity for itself (no interface registration,
  no Core message watcher). Added: `TREER_HUB_URL` (the hub's loopback
  listener), `TREER_HUB_JOB`, `TREER_HUB_TOKEN` (a fresh secret per spawn; only
  its SHA-256 is stored), `AGENT_UI_INSTANCE_ID`, and a Git identity
  `<hub>/jobs/<id>` unless the hub's environment sets one.
- **Interface:** the hub is a loopback native client of the worker's AIS:
  `POST /v1/prompts` (`queue`/`steer`, idempotent `operation_id`s
  `hub-<id>-first|-m<n>|-stop|-nudge|-resume-<instance>`), `GET /v1/status`
  (idle detection), `GET /api/health`, `GET /api/state` (session id),
  `GET /v1/transcript` (archive). A job description over 60 KiB is written to
  `STATE/workers/<id>/job-prompt.md` and the prompt points to it.
- **Handoff:** the worker POSTs to
  `$TREER_HUB_URL/api/jobs/$TREER_HUB_JOB/handoff` with
  `X-Hub-Job-Token: $TREER_HUB_TOKEN`. The token also reads that job's record
  (`GET /api/jobs/<id>`), nothing else, and is refused with browser headers.
- **End:** archive → SIGTERM to the worker's process group (the launcher stops
  its provider), SIGKILL after 15 s; the worker directory is emptied except
  `launcher.log`. The provider session stays in the provider's own storage.

Lifecycle (one supervisor loop, each step idempotent and retried):

1. `queued` → `starting`: when fewer than `--concurrency` (default 3) jobs are
   active, the hub spawns the worker.
2. → `running`: once its AIS reports ready, the first prompt is sent: task,
   inputs, input handoffs in full, the job id/address and the handoff command.
   The template's agent.md is **not** in the prompt; it is system-level
   instructions through `--instructions`.
3. → `handoff` → `archived`: the worker (or a human) submits the handoff; the
   hub exports the transcript, ends the worker process group and notifies the
   requester.
4. `failed`: the worker could not be started, never became ready or accepted
   its first prompt within `--start-timeout-seconds`, exited more than
   `--max-restarts` times (default 2; each crash is restarted on the same
   session with a "continue" prompt), stayed idle without a handoff for
   `--stall-seconds` after one reminder prompt, or `--max-runtime-seconds`
   elapsed. What exists is archived the same way.
5. `stopped`: `POST /api/jobs/<id>/stop` steers a STOP prompt asking for a
   `partial` interruption handoff, then archives after it arrives or after
   `--stop-grace-seconds`. A queued job stops immediately.

**Hub restart.** Stopping the hub (SIGTERM/SIGINT/SIGHUP) exports each live
worker's transcript and ends its process group. On start the hub first stops
any worker a crashed hub left behind (only one whose AIS answers with the
recorded instance id), then respawns every `starting`/`running` job on its own
`--state-dir` journal and session (`--session-id`), so accepted prompts are not
replayed, and sends a resume prompt telling the worker to continue and submit
its handoff. A job already in `handoff` is archived. Hub restarts do not count
as worker crashes. A worker whose hub died with SIGKILL keeps running until the
next hub start.

The completion notice is a Core Message from `/<hub>/jobs/<id>` to the
requester (job id, status, handoff summary, read/chain commands). An Agent
requester is also woken with `treer agent prompt`.

## App tree and pages

The hub implements [app-tree.v1](../../docs/app-tree.md):

```text
templates/<name>   app    page.read                       (agent.md + versions)
jobs/<id>          agent  message.send/read, page.read    while queued..handoff
jobs/<id>          app    message.read, page.read         once archived/failed/stopped
handoffs/<id>      app    page.read                       newest first
```

Messages sent to `/<hub>/jobs/<id>` (Console node view or
`treer message send --to`) are forwarded to the live worker's AIS as prompts
(a JSON body `{"text", "mode": "steer"}` steers); messages that arrive before
the first prompt are delivered right after it. A JSON job sent to the hub root
creates a job, at most once per Message; the reply names the job.

`page.read` nodes have a page at `/?node=<path>`: the Console embeds it in the
node view through the App UI tunnel. A running job's page is the worker's own
Agent UI; a finished job shows its handoff and transcript; templates and
handoffs show themselves. The full hub UI's job page embeds the worker UI too.

**Worker UI proxy.** `/workers/<id>/…` reverse-proxies the live worker's AIS
(HTTP and WebSocket upgrades, minimal stdlib passthrough) to
`127.0.0.1:<port>`, after verifying the caller's Treer identity like any other
request; mutations need a same-site browser request (no `X-Hub-Request`). The
hub rewrites Host to the worker's loopback address and drops Origin, Referer,
Cookie, Authorization, `Sec-Fetch-*`, `X-Treer-*` and `X-Forwarded-*`, so the
launcher sees a loopback native client. The launcher page uses only relative
URLs; `/workers/<id>` redirects to `/workers/<id>/`. A finished job answers
503 `worker_not_running`.

## Identity

Every `/api` request needs `X-Treer-Identity-Token`, verified with the Proxy's
`/.treer/apps/identity/verify` for this App's service and workspace (a verified
token is remembered for 20 s); humans and Agents are accepted. Managed Agents
get it automatically for `http://SERVICE_ID.treer/` from their runtime proxy
(which also sends it as `Authorization: Bearer`; any other Authorization is
refused). Human API mutations need `X-Hub-Request: 1` and a same-site Origin.
Only the job's worker (its `X-Hub-Job-Token`) or a human may submit a job's
handoff; the rest is cooperative (any workspace principal may create jobs, edit
templates, message, view or stop jobs).

The hub runtime's own Policy subject needs `message.send`/`message.receive`/
`message.ack` and `agent.prompt` (requester wake-ups); workers use the same
subject for their own `treer` commands. Callers need `identity.token.issue` for
the hub's service. A cooperative workspace Policy allows these.

## Deploy (macOS, e.g. mac_m2)

The `treer-launch` the hub runs must support `--instructions` and
`--no-register`; build one from this branch and point `--launcher` at it (an
absolute path):

```sh
cargo build --locked --release --manifest-path launchers/agent/Cargo.toml --target-dir target
mkdir -p dist/agent-hub && cp target/release/treer-launch dist/agent-hub/treer-launch
```

Create the App from a checkout (Host-relative `--cwd`) with a private state
directory outside the checkout:

```sh
treer app create --machine self --name agent_hub --cwd treer-agent-hub --port 8791 \
  --ui-capability app-tree.v1 python3 -- -B apps/agent-hub/app.py \
  --state-dir /Users/dooms/.local/state/agent-hub --port 8791 \
  --launcher /Users/dooms/treer-agent-hub/dist/agent-hub/treer-launch \
  --proxy-url https://proxy.canary.treer.ai/
treer app update /agent_hub --ui-capability app-tree.v1     # an existing deployment
```

A CLI without `--ui-capability` still works: the Console probes `api/tree`
and the hub's 401/JSON answer makes it expandable. Service ID, workspace and
address are discovered with `treer app list` / `treer agent show`
(`--service-id`, `--workspace`, `--app-name` override). Other flags:
`--jobs-root`, `--concurrency`, `--stall-seconds` (900),
`--stop-grace-seconds` (180), `--start-timeout-seconds` (600),
`--max-runtime-seconds` (43200), `--max-restarts` (2), `--treer`.

State: `hub.sqlite` (mode 0600), `instructions/`, `workers/<id>/` (launcher
journal, port file, log), `jobs/<id>/` working directories. One process lock
protects the state directory. Workers run on the hub's machine as the hub's OS
user with full tool access.

**Linux hosts.** `treer-launch` refuses a private state directory any of whose
ancestors is group- or world-writable (workers fail with
`private_state_unavailable`); Ubuntu's umask 002 creates such directories, so
`chmod 755` the directories above `--state-dir`. Host-started processes get a
fixed `PATH`, so start the App through `env PATH=... CARGO_HOME=... python3 ...`
when workers need a user-local toolchain; workers inherit the hub's environment,
including the workload `http_proxy` that reaches `*.treer` services such as the
Git Gateway.

**Upgrading from the Tree-Agent hub.** The schema is migrated in place. A job
that was still live with a real worker Agent is marked `failed` ("hub
upgraded") and that Agent is deleted with `treer agent admin delete`; the
`/agent-hub-workers` folder is no longer used and can be removed once empty
(`--folder` was removed).

## Validation

```sh
python3 -B -W error::ResourceWarning -m unittest discover -s apps/agent-hub/tests -v
```

The tests run the real HTTP server, SQLite and worker processes against a
synthetic launcher ([tests/fake_launch.py](tests/fake_launch.py): a minimal AIS
with a UI page and a WebSocket on a loopback port), a synthetic `treer` CLI
([tests/fake_treer.py](tests/fake_treer.py)) and a synthetic identity Proxy;
they do not exercise a real Core, Policy, launcher or provider.
