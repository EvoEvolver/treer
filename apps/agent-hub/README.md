# Agent Hub

A stdlib Python Managed App that runs **one task per fresh coding Agent**.
Long-lived Agents that do everything accumulate context; coding Agents work
best with a clean one. The hub creates a new Treer Agent for every job from a
versioned template, gives it only the task and its input handoffs, collects a
structured handoff, then archives the job and deletes the Agent so its memory
is released. Callers chain jobs themselves (`inputs.handoffs`); there are no
built-in pipelines and no approval steps.

The caller manual is [AGENT.md](AGENT.md) (served as Markdown at `/`); the
browser UI ([ui.html](ui.html)) uses the same HTTP API.

## Model

| Concept | Stored as |
| --- | --- |
| Template | `name`, immutable `version`s of `agent_md`, `harness` (`codex`/`claude`), optional `model`/`effort`, `cwd` (`job`, `home` or a Host-relative path). Bundled ones in [templates/](templates/) are seeded at first start and never overwritten. |
| Job | `j-0001`…: template@version, task, inputs (`handoffs`, `repo`, `branch`, `notes`), requester (verified Core principal), status, worker Agent ID/name/UI URL while live, provider session ID. |
| Handoff | Markdown with front matter (`job`, `template`, `status: done\|partial\|blocked`, `summary`, optional `branch`, `commits`) and sections *What changed*, *Verification*, *Open questions*, *Next steps*. Validated on submit. |
| Archive | the handoff, a `treer agent transcript` export, session ID and git refs from the handoff. |

Lifecycle (one supervisor loop, each step idempotent and retried):

1. `queued` → `starting`: when fewer than `--concurrency` (default 3) jobs are
   active, the hub writes the template version to
   `STATE/instructions/NAME@VERSION.md`, creates a job directory
   (`STATE/jobs/<id>` for `cwd: job`) and runs
   `treer agent admin create --machine self --kind command --name hub-<id> --folder /agent-hub-workers --cwd <dir> -- <launcher> --harness H --instructions FILE [--model M] [--effort E]`.
2. → `running`: once the worker's interface accepts it, the first prompt is
   sent: task, inputs, input handoffs in full, the job id/address and the exact
   handoff `curl`. The template's agent.md is **not** in the prompt; it is
   system-level instructions through `treer-launch --instructions`.
3. → `handoff` → `archived`: the worker (or a human) POSTs the handoff; the
   hub exports the transcript, deletes the Agent (`treer agent admin delete`),
   and notifies the requester.
4. `failed`: creation refused, never accepted its first prompt within
   `--start-timeout-seconds`, the Agent exited, it stayed idle without a handoff
   for `--stall-seconds` after one reminder prompt, or `--max-runtime-seconds`
   elapsed. What exists is archived the same way.
5. `stopped`: `POST /api/jobs/<id>/stop` steers a STOP prompt asking for a
   `partial` interruption handoff, then archives after it arrives or after
   `--stop-grace-seconds`. A queued job stops immediately.

The completion notice is a Core Message from `/<hub>/jobs/<id>` to the
requester (job id, status, handoff summary, read/chain commands). An Agent
requester is also woken with `treer agent prompt`.

## App tree

The hub implements [app-tree.v1](../../docs/app-tree.md):

```text
templates/<name>   app    page.read                       (agent.md + versions)
jobs/<id>          agent  message.send/read, page.read    while queued..handoff
jobs/<id>          app    message.read, page.read         once archived/failed/stopped
handoffs/<id>      app    page.read                       newest first
```

Messages sent to `/<hub>/jobs/<id>` (Console node view or
`treer message send --to`) are forwarded to the live worker with
`treer agent prompt` (a JSON body `{"text", "mode": "steer"}` steers). A
JSON job sent to the hub root creates a job, at most once per Message; the
reply names the job. `page.read` is this App's own marker: the human page for a
node is `/?node=<path>` (the same UI). The current Console node view shows
only Messages.

## Identity

Every `/api` request needs `X-Treer-Identity-Token`, verified with the Proxy's
`/.treer/apps/identity/verify` for this App's service and workspace; humans and
Agents are accepted. Managed Agents get it automatically for
`http://SERVICE_ID.treer/` from their runtime proxy (which also sends it as
`Authorization: Bearer`; any other Authorization is refused). Human mutations
need `X-Hub-Request: 1` and a same-site Origin. Only the job's worker Agent or
a human may submit a job's handoff; the rest is cooperative (any workspace
principal may create jobs, edit templates, message or stop jobs).

The hub runtime's own Policy subject needs `agent.create` on its machine,
`agent.tree.read`/`agent.tree.manage` (worker folder), `agent.metadata.read`,
`agent.prompt`, `transcript.read`, `agent.stop`/`agent.delete`, and
`message.send`/`message.receive`/`message.ack`; callers need
`identity.token.issue` for the hub's service. A cooperative workspace Policy
allows these.

## Deploy (macOS, e.g. mac_m2)

The installed `treer-launch` must support `--instructions`; build one from this
branch and point `--launcher` at it (an absolute path):

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
`--folder` (default `/agent-hub-workers`), `--jobs-root`, `--concurrency`,
`--stall-seconds` (900), `--stop-grace-seconds` (180),
`--start-timeout-seconds` (600), `--max-runtime-seconds` (43200), `--treer`.

State: `hub.sqlite` (mode 0600), `instructions/`, `jobs/<id>/` working
directories. One process lock protects the state directory. Workers are full
Agents on the hub's machine with the same access as any Agent there.

## Validation

```sh
python3 -B -W error::ResourceWarning -m unittest discover -s apps/agent-hub/tests -v
```

The tests run the real HTTP server and SQLite against a synthetic `treer` CLI
([tests/fake_treer.py](tests/fake_treer.py)) and a synthetic identity Proxy;
they do not exercise a real Core, Policy or provider.
