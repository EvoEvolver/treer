# Agent launcher local security audit — 2026-10-12

Scope: `launchers/agent` UI/AIS, native/ACP adapters, file reads, session archives
and pending message watcher `8db1be2`. Baseline main `3372b34`. Only disposable
loopback listeners, fake Codex/Claude/ACP and synthetic files were used. No
Canary/user-machine requests, provider login, real session files or credentials
were read. P0 means unauthorized execution/data exposure reachable through a
browser/remote origin, P1 significant availability/confidentiality risk, P2
documented trust limitations or defense-in-depth. Internet-origin reachability
depends on browser local-network restrictions; no universal browser exploit or
machine sandbox claim is made.

## Findings, ordered by severity

### P0 — Cross-origin prompt injection and WebSocket transcript disclosure (open)

- **Phenomenon:** `/api/prompt`, interrupt/settings/goal, `/v1/abort` and `/ws`
  have no general authentication or Origin check. `text/plain` POST is parsed as
  JSON without a preflight. WebSocket upgrades emit state/history immediately.
- **Local reproduction:** from `launchers/agent`, run
  `node tests/audit-local-surface.mjs`. A disposable browser on a second loopback
  origin opens the fake Agent's WebSocket and submits a synthetic prompt. Output
  confirmed `crossOriginWebSocketStateRead=true` and
  `crossOriginSimplePostReachedTurn=true`. It outputs booleans, never contents.
- **Impact:** a browser origin that can reach the port can read conversation
  state or submit work with the Agent's account authority. Port discovery and
  browser local-network restrictions are practical prerequisites. No real
  shell commands were executed; fake `turn/start` receipt proves unauthorized
  dispatch. This is not protected by Proxy Policy on the direct loopback path.
- **Recommendation:** exact loopback Host and same-origin validation for direct
  HTTP/WS, including Origin `null` and cross-site simple requests; reject unsafe
  requests before reading their body. Tunnel traffic needs an authenticated
  Controller forwarding marker, not an arbitrary Origin allowlist.
- **Size/status:** medium cross-component change, **not repaired by this audit**;
  core-dev contacted to define the private forwarding contract. A blanket
  Origin rejection would break the real iframe/WS tunnel and is not a safe fix.

### P0 — DNS-rebinding Host bypass exposes project reads/state (open)

- **Phenomenon:** most routes accept arbitrary Host. Only session export checks
  loopback Host/no Origin. `/v1/files` can read `.env` if inside cwd.
- **Local reproduction:** the same script sends `Host: rebinding.invalid` to the
  temporary listener and reads an invented `.env`; confirmed
  `foreignHostReadSyntheticProjectFile=true`. This simulates the server-side
  rebinding condition; **no DNS service or real rebinding attack was performed**.
- **Impact:** a browser resolving its origin to loopback could read project
  contents/state where browser DNS/private-network protections permit it.
- **Recommendation:** bind Host to exact local listener origins for direct
  traffic; authenticate tunnel exceptions in HTTP and WS before serving data.
- **Size/status:** same coordinated fix as above, remains open.

### P1 — Unbounded request buffering; malformed JSON may leak request fragments (fixed here)

- **Phenomenon:** `readJson` collected the entire stream before parsing; Node
  JSON parse errors can contain raw request fragments, and the catch handler
  logged/returned them. WebSocket accepted its default large inbound payload.
- **Local reproduction:** `tests/security-http.test.ts` sends a synthetic secret
  in invalid JSON, declared/chunked >128 KiB bodies and >16 KiB WS data. Former
  unbounded/parser behavior follows baseline source; tests now require generic
  400, no marker in logs/response, 413, WS close 1009 and server survival. No
  memory exhaustion attack was run.
- **Impact:** reachable callers could consume unbounded memory or cause request
  content to enter crash/request logs; unauthorized reachability is as above.
- **Recommendation/change size:** small, implemented streaming 128 KiB cap,
  object-only JSON and generic parse errors, request logs limited to error
  class, 16 KiB WS maxPayload and socket-error handling. Limits do not bound
  concurrent connections, cumulative history or model output.
  An additional small fix guards malformed absolute URLs in HTTP/WS before
  routing: `http://%` previously escaped the request try/catch. The sealed
  regression now requires 400 and verifies the process still serves state.

### P1 — Loopback UI/AIS lacks independent local-client authentication (open design boundary)

- **Phenomenon:** direct local `/api/state`, transcript/files/prompt routes do
  not require a bearer token; queue attestation uses public Agent/instance IDs.
  `/v1/manifest` discloses instance ID. Host/Origin checks alone cannot separate
  native local processes, which may supply their own headers.
- **Local reproduction:** sealed fake server responds to unauthenticated state
  and file requests in the audit script. Code review confirms metadata checks
  only, not proof of Controller possession.
- **Impact:** other local users/processes able to reach loopback have Agent
  read/control authority. Same-account access is explicitly unsupported, but
  loopback is also shared across accounts absent external network isolation.
- **Recommendation:** private per-registration forwarding secret, checked on
  all sensitive routes; authenticated explicit direct-local mode if required.
  Public manifest/health may remain minimal discovery routes. Never expose the
  workload credential to browser or forward it as the AIS capability.
- **Size/status:** medium; coordinate Core-generated registration capability,
  Proxy stripping/Controller overwriting browser headers and launcher checks.

### P2 — Archive allowlist is not redaction; secrets in history/memory survive

- **Phenomenon:** native JSONL and memory/instruction contents export verbatim;
  base64 and hashes are encoding/integrity, not encryption or sanitization.
- **Local reproduction:** both native harness tests insert an invented marker
  into temporary JSONL/MEMORY.md and confirm it appears after decoding export.
  Existing tests verify auth/config/.env/shell snapshots stay out and imports
  reject traversal, mismatched hashes, symlinks and unsafe destinations.
- **Impact:** sharing a valid archive can share secrets previously put in a
  conversation or memory; restore also trusts imported instructions. Codex
  restore references local provider auth/config via symlinks, not an isolated
  provider identity. Claude restore uses the local provider's project store.
- **Recommendation/size:** small truthful docs (done); protect archives as
  sensitive data and use a user-reviewed redacted export for sharing. General
  native-history redaction is large and can break resume; not claimed or added.

### P2 — Full-authority harnesses inherit credentials and raw output (intentional)

- **Phenomenon:** Codex `approvalPolicy=never`, `danger-full-access`; Claude
  `--dangerously-skip-permissions`; ACP auto-approves requests and selects a
  full-access mode. Child spawn inherits environment, including own `TREER_*`
  workload credentials needed by Treer tools. Provider stderr goes to runtime
  logs; tool results/conversations/notification previews are not redacted.
- **Evidence/reproduction:** fake-adapter tests assert startup permission
  flags/policy. Code review of each spawn and stderr handler confirms inherited
  environment/output; **no real environment values were read or logged**.
- **Impact:** malicious tools/tasks can use that Agent identity and read files
  available to the account. Commands may reveal secrets in transcripts/logs.
- **Recommendation/size:** docs and explicit user warning added (small). Use
  separate account/container/VM for untrusted tasks. Removing all credentials
  from children would break delegated Treer commands and would not protect them
  from same-account filesystem/process inspection. Secret scanning would be
  best-effort, not a guaranteed redaction claim.

### P2 — Message watcher labels untrusted content but cannot prevent prompt injection

- **Phenomenon:** the approved watcher wraps all kinds, including completion
  notices, with sender kind/path/message ID and an explicit non-system warning;
  32 KiB body truncation keeps the warning/retrieval command. It always queues,
  disables automatic terminal notification feedback and validates own recipient.
- **Local reproduction:** fake watcher tests cover wrapping, newline escaping,
  Unicode truncation, FIFO/busy behavior, restart idempotence and durable ack.
  Sender body is never interpreted as trusted launcher instructions.
- **Impact:** a model can still follow adversarial message content. Queueing and
  identity attribution are not an LLM instruction-safety boundary. Permitted
  senders can fill the bounded queue; no sender-specific quotas here.
- **Recommendation/size:** keep warning and source attribution, allow
  `AGENT_UI_MESSAGE_WATCH=0`, constrain Core Message Policy and execution account.
  Reliable semantic filtering is unsupported and explicitly not introduced.

### P2 — File containment is not a same-account race-proof sandbox

- **Phenomenon:** lexical `..`/absolute escape and canonical external symlinks
  are rejected; previews open canonical paths with O_NOFOLLOW/O_NONBLOCK and
  verify regular files. Intermediate directories can still be swapped between
  resolution and open by an account with write authority. Cwd contains no
  secret-file denylist; authorized preview of `.env` is expected.
- **Local reproduction:** existing `ux.test.ts` covers static path traversal,
  internal/external symlinks, special files and 64 KiB caps. No successful
  static traversal found. Intermediate-directory race is **source analysis,
  not a reproduced exploit**; this audit does not claim race-proof confinement.
- **Impact:** concurrent same-account mutations can defeat a pathname check;
  Agent commands already hold that account's file authority.
- **Recommendation/size:** maintain current checks; stronger dirfd/openat-style
  containment or separate filesystem namespace is medium/platform-specific.

## Verification and release implications

Run `npm test`, `npm run check`, `npm run test:browser` and
`node scripts/check-docs.mjs`. Repro script is intentionally separate from
regression tests; its booleans should become false after the coordinated P0 fix.
Regression tests for resource bounds/redacted parse errors do not weaken existing
argv/session/file assertions. No push, deployment or Agent/Host/Controller restart.

This audit does **not** approve deployment on a critical service with mutually
untrusted users or an exposed listener. Loopback binding, external Proxy Policy,
file allowlists and message provenance are useful basics, but do not establish
authenticated local UI access, browser CSRF/rebinding protection, secret-free
archives/logs, provider credential isolation or an execution sandbox. Update
`docs/security.md` Unsupported Claims with these exact limitations; its owner
core-dev was asked to synchronize this cross-component wording.
