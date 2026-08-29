# Machine connection UX execution plan

- Status: completed
- Started: 2026-08-28
- Completed: 2026-08-28
- Branch / worktree: `feat/machine-connection-ux` at `/Users/mac/dev/treer-machine-connection`

Maintained current-state documents remain authoritative until each phase
ships. This plan is the delivery sequence, not a description of shipped
behavior.

## Goal

A workspace member can tell whether a machine is running locally, connected to
the control plane, fenced as a duplicate, or stopped — and can recover without
guessing workspace IDs or re-enrolling. Lid-close sleep, Proxy restarts, and
stale WebSockets must reconnect by themselves. `proxy-env` (macOS default) must
not send ordinary internet traffic through the Treer path, so GitHub/`gh` keep
working when the control-plane socket is down.

## Observed failures this plan covers

1. Webpage Offline while Host+Controller still run. Local
   `treer-agent-server service start` printed "Proxy connection are ready"
   because it only probed loopback `/api/health` and `/api/agents`.
2. After `duplicate_machine_connection`, the Controller **stops reconnecting
   forever** (`ConnectionDisposition::StopDuplicate` breaks `run_forever`)
   while the process stays up. The UI has no copyable restart command.
3. macOS lid close pauses the process. The Proxy keeps a half-open socket and
   still advertises Online. On wake the frozen connection can receive
   `duplicate_machine_connection` and abort reconnects, or CONNECT/SOCKS waits
   on the dead socket so Agent `gh auth status` looks like a per-Agent login
   failure.
4. One Mac can hold several enrollments (`Mac.home.com` × multiple
   `server_id`s). CLI `--workspace` defaults to the literal string `default`,
   which matches none of them.
5. `proxy-env` injects `ALL_PROXY` / `HTTP(S)_PROXY` for **every** destination.
   Classification still round-trips to the Proxy (`NetworkBinaryKind::Open`)
   even for `api.github.com`. Control-plane disconnect therefore times out
   GitHub.

## Decisions

### Status model

Publish four machine connection states. Webpage `online` stays the only
fully-healthy badge; the other three are Offline variants with a reason.

| State | Source of truth | User action |
| --- | --- | --- |
| `online` | Proxy lease matches this Controller instance | none |
| `local` | Host+Controller listen locally; no current Proxy lease | reconnect / wait |
| `fenced` | This process was told another connection owns the `server_id` | restart Controller (or wait for automatic reclaim) |
| `stopped` | No supervised Host process | start the service |

Do not call local `/api/agents` "Proxy ready". Ready means: local health **and**
the Proxy still lists this `controller_instance_id` as the owner (or a
dedicated `/api/health` field `proxy_connected`).

### Reconnect, including lid close

- Ordinary close, RST, 502, Proxy recreate, and **sleep/wake** all stay in the
  reconnect loop with capped exponential backoff (keep today's 300ms → 5s
  unless tests show wake needs a longer first delay).
- `duplicate_machine_connection` and `stale_connection` are **retryable**. The
  loser backs off and reconnects until it is owner again, the credential is
  revoked, or the server is deleted. Never `break` out of `run_forever` for
  duplicate.
- Proxy: WebSocket ping/pong (target: ping ≤ 20s, dead ≤ 60s) so a sleeping Mac
  is dropped before the laptop wakes into a zombie Online row.
- On Unix `SIGCONT` (lid open / thaw), if the last successful pong is older
  than the dead interval, close the socket and reconnect immediately.
- Replacement rule stays: the newest authenticated connection owns the
  `server_id`; the previous socket is closed. That is how a waking Mac takes
  over from its own frozen connection.

### One Host per hostname + workspace

- `connect` for an already-installed `(hostname, workspace_id)` reuses the
  existing `server_id` and credential. It must not mint a second machine.
- Starting a Controller when that `server_id` already has a live listen socket
  is a hard error that prints pid and address.
- CLI with no `--workspace`: list every local install; if exactly one exists,
  use it; if several, fail with the table. Remove the implicit workspace name
  `default` from `treer-agent-server service`.
- Webpage: multiple machines that share a hostname show as multiple installs,
  with `server_id` suffix and listen port.

### `proxy-env` is not a default internet proxy

Virtual-host names have no reserved suffix, so `NO_PROXY=*` cannot express
"everything except workspace aliases". Classification must be local:

- The Controller already receives a revisioned virtual-host snapshot.
- If the CONNECT/SOCKS destination is **not** in that snapshot and is not the
  reserved local-API address `192.0.2.1`, dial **this machine** immediately
  (`NetworkBinaryKind::Direct` locally, no Open RPC, no wait on the Proxy
  socket).
- If it **is** a snapshot virtual host (or `192.0.2.1`), keep today's Treer
  path.
- Disconnect / `reset_all` may tear down **relayed** streams only. Direct
  internet sockets and the CONNECT listener stay up so `gh`, npm, and curl
  survive Offline.
- Keep injecting `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` so `curl http://api.internal`
  still works; the local listener is a classifier, not a choke point for
  GitHub.
- Optional later: `TREER_NETWORK_MODE=proxy-env-split` that omits `HTTP_PROXY`
  entirely and only wraps Git via `GIT_PROXY_COMMAND`. Not required to close
  this incident.

Linux `transparent` is unchanged: TUN still captures all TCP. This phase is
macOS / `proxy-env` only.

## Delivery

Ship in order. Each phase is mergeable. Do not start phase *n+1* until phase
*n* tests and its listed doc updates land. After every phase run
`node scripts/check-docs.mjs` plus the phase's focused tests. `just check`
before calling the plan complete.

### Phase 1 — Truthful local status and copyable recovery

**Intent.** Operators can see why a machine is Offline and copy the exact
command for this workspace.

**Code**

- `treer-agent-server service status` with no workspace lists installs
  (`workspace`, `server_id`, `listen`, `proxy`, LaunchAgent/systemd state,
  `proxy_connected` if known).
- `service start|stop|restart|restart-controller|repair|logs` refuse
  `--workspace default` unless such a workspace exists. Zero installs: print
  the Add-machine enroll hint. Several installs: print the table and exit 2.
- `wait_for_controller_and_proxy` requires a Proxy-lease signal, not only
  `/api/agents`. Add `proxy_connected` (and last error) to `/api/health`.
- Webpage machine card and the Agent Offline panel: reason string + copy
  buttons for `start` and `restart-controller` with the real workspace id.
  Add machine dialog stays enroll-only; do not reuse it for reconnect.

**Tests**

- Service CLI: no-arg list; unknown `default`; unique workspace implied.
- Health JSON includes `proxy_connected`.
- Frontend typecheck/build; desktop/mobile Offline panel copy.

**Docs when this phase ships:** `README.md` (service examples),
`skills/treer/SKILL.md` (operator recovery), this plan (mark phase 1 done).

**Done when:** `service start` cannot print ready while the webpage is Offline
for a fenced Controller; Offline UI shows a pasteable `restart-controller`
line that includes `ws_…`.

### Phase 2 — Reconnect through sleep, Proxy bounce, and duplicate

**Intent.** Lid close, 149 Proxy recreate, and duplicate fencing cannot leave a
live Controller that will never dial again.

**Code**

- Remove `StopDuplicate` as a terminal loop exit. Map duplicate and stale to
  reconnect with backoff.
- Proxy WebSocket ping/pong + idle close (numbers in Decisions). Tests for
  dead-peer eviction.
- Controller: on `SIGCONT`, if last pong is stale, abort the current socket and
  reconnect. Test with a fake clock / injected signal handler where possible.
- Register-server replacement already sends duplicate to the previous socket;
  keep that. Add a test: owner A frozen, owner B (same credential, new
  instance) connects, A is closed, B stays; A later reconnects and becomes
  owner if B is gone.
- Sleep scenario fixture (can be unit/async): half-open connection + replacement
  + loser retries instead of exiting.

**Tests**

- `crates/treer-agent-server` reconnect dispositions.
- `crates/treer-proxy` lease replacement, ping timeout, duplicate-is-not-delete.
- Do not require a physical lid close in CI. Record a manual macOS sleep/wake
  check in the phase result.

**Docs when this phase ships:** `docs/architecture.md` (reconnect/lease;
sleep), `docs/quality.md` (Host/Controller lifecycle evidence), this plan.

**Done when:** a Controller that receives `duplicate_machine_connection` still
has an active reconnect loop; Proxy drops a silent socket within the dead
interval; a waking Mac can become Online without `service restart`.

### Phase 3 — One supervised Host per hostname and workspace

**Intent.** `Mac.home.com` in one workspace is one machine.

**Code**

- `connect`: if `(install hostname, workspace)` already has config, reuse
  `server_id` and credential; do not create `srv_` twins for the same pair.
- Bind/listen: fail if the configured listen address is already owned by a
  live Treer Controller for that `server_id`.
- Webpage: machines sharing a hostname in one org/workspace show install
  identity (`srv_…` tail, port, root). Optional doctor command aggregates
  local installs.

**Tests**

- Connect reuse; second process bind error.
- Snapshot JSON for hostname collision labeling if the field is new.

**Docs when this phase ships:** `README.md` (connect reuse),
`docs/architecture.md` (installation identity), this plan.

**Done when:** a second enroll of the same host into the same workspace cannot
create a second LaunchAgent; CLI without `--workspace` no longer searches for
`default`.

### Phase 4 — `proxy-env` classifies locally; public internet bypasses Treer

**Intent.** `gh`, npm, and curl to the public internet work in Agent PTYs even
when the machine is Offline. Workspace virtual hosts still go through Treer.

**Code**

- CONNECT/SOCKS handler: destination in virtual-host snapshot or `192.0.2.1`
  → existing Open/relay path. Otherwise local `TcpStream::connect` immediately.
- `reset_all` on Proxy disconnect only resets relayed stream maps, not the
  listen socket and not in-flight Direct copies.
- Tests: `api.github.com` Direct with Proxy websocket closed; `api.internal`
  still Open when present in the snapshot; unknown host does not send Proxy
  data frames (extend `direct_route_bridges_locally_without_proxy_data_frames`).
- Do not require operators to maintain a public-suffix `NO_PROXY` list.

**Tests**

- Existing `http_connect_*` plus a "Proxy down, Direct still succeeds" case.
- macOS-focused; Linux transparent tests must stay green and unchanged.

**Docs when this phase ships:** `README.md` workspace network, `docs/architecture.md`
(proxy-env), `docs/security.md` (proxy-env is not a full intercept), this plan.

**Done when:** with the Controller WebSocket stopped, `curl -x $HTTPS_PROXY
https://api.github.com/user` returns an HTTP response instead of a connect
timeout; a configured virtual host still relays.

### Phase 5 — Docs, skill, and close-out

**Intent.** Maintained documents match shipped behavior; the plan is marked
completed.

**Code:** none beyond doc and test-name leftovers.

**Docs:** product (machine promise), architecture, security, quality evidence
row for connection UX, README operator flows, `skills/treer/SKILL.md`, this
file's Result section. Move this plan to Historical in `docs/README.md`.

**Done when:** `just check` passes, including `node scripts/check-docs.mjs`,
and no phase checkbox below remains open.

## Phase checklist

Use this as the implementation order. Check a box only after that phase's
**Done when** is true in source and tests.

- [x] Phase 1 — Truthful local status and copyable recovery
- [x] Phase 2 — Reconnect through sleep, Proxy bounce, and duplicate
- [x] Phase 3 — One supervised Host per hostname and workspace
- [x] Phase 4 — `proxy-env` local classification / internet bypass
- [x] Phase 5 — Maintained docs and `just check`

## File map (expected)

Not exhaustive; adjust in the implementing commit if ownership differs.

| Phase | Primary files |
| --- | --- |
| 1 | `crates/treer-agent-server/src/service.rs`, `local_api.rs`, `web/src/App.tsx`, `web/src/lib/api.ts`, `crates/treer-cli/src/main.rs` |
| 2 | `crates/treer-agent-server/src/proxy.rs`, `crates/treer-proxy/src/state.rs`, `crates/treer-proxy/src/agent_socket.rs` |
| 3 | `crates/treer-agent-server/src/service.rs`, `main.rs`, `web/src/App.tsx` |
| 4 | `crates/treer-agent-server/src/network.rs`, `controller.rs` (`reset_all` callers) |
| 5 | `docs/*`, `README.md`, `skills/treer/SKILL.md` |

## Verification

| Phase | Focused evidence |
| --- | --- |
| 1 | CLI unit/integration around service listing; health JSON; web typecheck/build |
| 2 | Proxy lease/ping tests; Controller reconnect disposition tests; manual macOS sleep/wake note |
| 3 | Connect reuse and bind-conflict tests |
| 4 | Direct-vs-vhost CONNECT tests with websocket closed |
| 5 | `just check` |

Do not point `TREER_TEST_DATABASE_URL` at a shared database. When Docker is
unavailable, run the focused crate tests and record the skipped PostgreSQL
gate.

## Result

Shipped on `feat/machine-connection-ux`:

- Local `/api/health` publishes `proxy_connected`, `connection_state`
  (`online` / `local` / `fenced`), and last Proxy error. `service start`
  waits on that lease, not `/api/agents`.
- `treer-agent-server service` no longer implies workspace `default`.
  Status with no `--workspace` lists installs; other commands use a unique
  install or print the table and exit 2. Offline web cards copy
  `restart-controller` / `start` with the real `ws_…` ID.
- `duplicate_machine_connection` and `stale_connection` stay in the
  reconnect loop. Proxy WebSocket ping ≤ 20s, dead ≤ 60s. Unix `SIGCONT`
  aborts a stale socket. Newest authenticated connection owns the
  `server_id`.
- `connect` reuses the installed hostname+workspace `server_id`. A second
  Controller for a live listen socket is a hard error with pid/address.
  Hostname collisions in the webpage show `srv_` suffix and listen port.
- `proxy-env` dials non-virtual-host destinations locally without Open
  RPC. `reset_all` only resets relayed streams.

Remaining manual check: macOS lid close / open on a LaunchAgent Host, confirm
the webpage returns Online without `service restart`, and
`curl -x $HTTPS_PROXY https://api.github.com/user` still returns HTTP while
the Controller WebSocket is stopped.

PostgreSQL-backed Proxy tests require `TREER_TEST_DATABASE_URL` or Docker;
when those are unavailable, run the focused crate tests and record the
skipped gate.
