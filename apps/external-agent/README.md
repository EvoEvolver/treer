# External Agent App

Lets programs outside Treer — Claude Code, Codex, scripts, CI — act inside a
workspace. The App issues them bearer tokens; every call runs as this App's
own Treer Agent, so Core Policy for that Agent decides what is allowed. The App
has no grant engine of its own and never acts as a human.

What an external program gets:

- **Device-code onboarding.** The program asks for a short code; a workspace
  owner approves it on the settings page; the program receives its token.
- **The real Treer CLI.** `POST /v1/cli` runs the native `treer` CLI with argv
  only (no shell), as this App's Agent.
- **Its own inbox.** A registered key `KEY` is addressable as `APP_ID/KEY`;
  teammates `treer message send --to APP_ID/KEY`, the program polls and acks
  through the Core App-endpoint inbox.

The Agent-facing reference is served at `/` (Markdown) and `/SKILL.md`; source
in [`src/runtime/manual.md`](src/runtime/manual.md) and
[`src/runtime/agent-skill.md`](src/runtime/agent-skill.md).

## Two ports

| Config | Audience | Routes |
| --- | --- | --- |
| `listen` | External programs (public data service) | `/health`, `/v1/...` |
| `owner_listen` | Workspace owners and Agents (the Managed App's primary, workspace-authenticated service) | `/`, `/owner/...`, `/api/tree` |

Each port returns 404 for the other's routes; selection is by socket, never a
header. Publish only `listen` as the public data service.

## Management API: humans and Agents

Settings page and Agents use the same `/owner/*` API. Callers present
`X-Treer-Identity-Token` for this App's service (`config.service`), verified
through the Proxy on every request:

- **Human:** must be a current workspace owner. Browser mutations also need a
  configured `Origin`, same-origin `Sec-Fetch-Site` and `X-External-Agent-CSRF: 1`.
- **Agent:** any workspace Agent whose runtime proxy obtained an identity for
  this service — Core issues one only when the Agent's Policy allows
  `identity.token.issue` on the service, so that is the grant to manage. The
  App's own execution identity is refused (external tokens act as it).
- **Device-code approval** (`POST /owner/device/decision`) stays a single human
  yes/no; Agents get `403 human_owner_required`. Agents can list
  (`GET /owner/device/requests`) and look up requests, and can issue keys
  directly with `POST /owner/external-agents`, so they never need a human to
  onboard a program they control.

Agents can create/list/revoke tokens, register external agents, read the audit,
and enable/disable the external API. The route table is in
[`manual.md`](src/runtime/manual.md).

## Requirements and readiness

- **Endpoint inbox:** needs the Controller to advertise
  `message.endpoint-inbox.v1` in `/api/health` `cli_capabilities`
  (`POST /api/messages/receive|ack` with `app_endpoint`). If it is absent the
  App still starts; `/v1/inbox*` returns `503 inbox_unsupported`, and
  `/owner/state` `readiness.inbox` and `/owner/integration` blockers say why.
  The App re-checks on the next request, so a Treer upgrade needs no restart.
- **CLI gateway:** `treer_cli` is the absolute path of the native CLI. It is
  identified by running `treer_cli --version` (expects `treer X.Y.Z`), never
  by hash, so Treer updating the binary in place keeps working. Without
  `treer_cli`, or if the probe fails, `/v1/cli` returns `503 cli_unavailable`
  with the reason, also shown in `readiness.cli`.

## Run

```sh
cargo build --release --locked --manifest-path apps/external-agent/Cargo.toml
treer-external-agent /private/external-agent/config.json
```

Run it as a Managed App (it checks `TREER_WORKSPACE_ID`, `TREER_AGENT_ID`,
`TREER_SERVER_ID`, `TREER_AGENT_SERVER_URL` against the config and uses
`TREER_WORKLOAD_CREDENTIAL`). The config is a private 0600 JSON file:

```json
{
  "workspace": "ws_example",
  "app": "app_example",
  "service": "svc_owner_service",
  "agent": "ag_0123456789abcdef0123456789abcdef",
  "machine": "machine_example",
  "controller": "http://127.0.0.1:8790/",
  "proxy": "https://proxy.example/",
  "public_origin": "https://external-data.example/",
  "owner_origin": "https://external.example/",
  "console_origin": "https://console.example/",
  "tunnel_origin": "https://proxy.example/",
  "console_url": "https://console.example/workspaces/ws_example/",
  "listen": "127.0.0.1:8080",
  "owner_listen": "127.0.0.1:8081",
  "state": "/private/external-agent/state",
  "treer_cli": "/home/me/.local/bin/treer"
}
```

`public_origin` is the data URL shown to external programs. `service` is the
owner service audience. `state` is a private 0700 directory outside version
control. The retired `policy_acceptance_sha256` / `prompt_review_sha256`
fields are still accepted and ignored. Config edits never invalidate tokens:
verifiers are keyed by a random per-store secret, and the store records the
workspace/App/Agent identity it belongs to, refusing to open for another one.

The external API starts disabled; an owner or Agent enables it with
`POST /owner/activation`.

## State and security notes

- SQLite in `state` (0600 files, schema v6; older v1–v5 stores upgrade in place,
  keeping tokens, generation and audit). Only a verifier of each token is
  stored; plaintext is returned once.
- Every external call reserves a body-free audit row before dispatch and is
  never retried; a lost result is recorded as `outcome_unknown`.
- Tokens expire after 30 or 90 days, share the App's authority, and are
  rate-limited (60/min, 2 concurrent per token, 8 total). Revocation applies to
  the next request; an already running command may finish.
- If the management port is unreachable, stop the App from the Console or
  Controller; that cuts off all external access at once.
- This is not isolation from other processes under the same OS account.

## Verify

```sh
cd apps/external-agent
cargo fmt --check && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked
python3 tests/smoke.py --binary target/debug/treer-external-agent [--browser]
EXTERNAL_PLAYWRIGHT_MODULE=/abs/node_modules/playwright/index.mjs node tests/owner-device-browser.mjs
```

`smoke.py` runs the real executable against loopback fixtures and a fake CLI;
the `tests/*.mjs` files drive the settings UI in Chromium with synthetic data.
