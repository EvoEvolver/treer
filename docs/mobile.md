# Treer mobile clients

- Status: maintained
- Last source review: 2026-09-01

Native iOS and Android apps live under `mobile/`. They are clients of the same
Proxy contract as the browser and CLI. They are not workspace Apps: do not
place them in `apps/`.

## Product surface

The apps are a **fleet orchestrator**, not a Codex thread list.

- Bottom tabs: Home (attention / working / idle), Machines, Inbox (Agent
  attention only).
- Persistent Voice button opens a near-fullscreen sheet: a scrolling
  `user:` / `assistant:` transcript. Two input modes: hold-to-talk, and
  conversation. Conversation runs a local RMS + zero-crossing gate plus a
  360ms/900ms speech/silence window so coughs and clicks are not uploaded;
  ASR starts only after speech is confirmed. Assistant replies are read with system TTS
  (media stream). If the engine, voice data, or volume is missing, the sheet
  shows an install/settings prompt instead of failing silently.
- Agent thread rendering uses a WebView hosting the bundled
  `codex-agent-ui` web assets, forwarded to that Agent's AIS tunnel.
- Emergency TUI is a native PTY overlay. It is off by default.
- Machines tab `+` opens **Add machine**. That screen `POST`s
  `/api/workspaces/{id}/bootstrap` and shows the install and connect commands
  to copy onto a computer. The phone does not run a Host; the machine appears
  in the snapshot when `treer-agent-server connect` succeeds.
- Home tab `+` opens **Assign**. Create Agent lists AIS-capable launch profiles
  first, then catalog TUI kinds (including Codex), then a built-in Terminal
  (`kind=command`). Recipe install is not available on mobile. Empty-machine
  Assign links to Add machine instead of a dead end.

See the dated product scheme in
[research/2026-08-30-mobile-app-plan.md](research/2026-08-30-mobile-app-plan.md)
for screen-level empty/error/offline copy. This file records shipped contracts.

## Authentication

Browser login still sets the HttpOnly `treer_session` cookie and returns JSON
without a token.

Native login and register send `X-Treer-Client: mobile`, `mobile_ios`, or
`mobile_android`. That header is **not** in CORS `allow_headers`. On an exact
match, `session_response` includes `token` in the JSON. Web clients that omit
the header keep the cookie-only body.

`authenticate_request` accepts `Authorization: Bearer` or the session cookie on
every `require_user` route, including workspace events, terminal WebSocket, and
AIS UI tunnels. The public contract does not put the session on a query string.

Login/register JSON may include a client-generated `device_id` (UUID) and
`device_name`. Logout deletes that session row.

Tokens live in Keychain / Android Keystore, not ordinary preferences.

## Abort

`POST /api/workspaces/{workspace_id}/agents/{agent_id}/abort` (and the matching
`/agent/...` route) forwards to AIS `POST /v1/abort` when the Agent advertises
the `abort` capability. Policy action is `agent.abort`, distinct from
`agent.prompt` and `agent.stop`. Missing capability returns
`agent_interface_capability_unavailable`.

## Thread WebView

`mobile/agent-ui/` is a generated Vite bundle of `codex-agent-ui`'s web UI
(`~/dev/codex-agent-ui/apps/web`). It is gitignored. Native shells load those
assets locally after `just mobile-bundle-ui` and forward relative HTTP/WebSocket
to

`/api/workspaces/{workspace_id}/agents/{agent_id}/interface/ui/`.

The shell injects the session cookie for the Proxy host into the WebView cookie
store. The page must not receive the Bearer token in `window` or the URL.

Agents without an AIS `ui_path` stay on the native Agent detail composer and
optional terminal. Do not native-render Codex/Pi chat timelines.

## Build

Native projects are **not** part of `just check`. Focused commands:

```sh
just mobile-ios-ci
just mobile-android-ci
```

Rebuild the bundled UI after changing `codex-agent-ui`:

```sh
just mobile-bundle-ui
```

## Voice ASR (opt-in)

Hold-to-talk is enabled when the **Proxy process** has
`TREER_VOICE_ASR_PROVIDER=qwen` and `TREER_VOICE_ASR_API_KEY` or
`DASHSCOPE_API_KEY` (shell env, Compose `.env`, or the process supervisor). Do
not put these in the iOS/Android apps. The native shell streams
16 kHz PCM16 over

`GET /api/workspaces/{workspace_id}/voice/asr`

and

`GET WS /api/workspaces/{workspace_id}/voice/asr/stream`

(`Authorization: Bearer`). Audio is forwarded to Qwen
`qwen3-asr-flash-realtime` and is not stored in Core. Optional
`TREER_VOICE_ASR_URL` (default Singapore
`wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime`) and
`TREER_VOICE_ASR_MODEL`. Unconfigured deployments keep the Preview empty
state. Use a Beijing key only with `wss://dashscope.aliyuncs.com/api-ws/v1/realtime`;
keys are region-specific.

## Voice command (opt-in)

ASR text is turned into Treer actions on the **Proxy**, not the phone. Enable
with `TREER_VOICE_LLM_API_KEY`. Optional:

- `TREER_VOICE_LLM_BASE_URL` (default `https://sub.lnz-study.com`)
- `TREER_VOICE_LLM_WIRE_API` (`responses` or `completions`, default `responses`)
- `TREER_VOICE_LLM_MODEL` (default `gpt-5.6-luna`)

The native shell posts the utterance to

`GET /api/workspaces/{workspace_id}/voice/command`

and

`POST /api/workspaces/{workspace_id}/voice/command`

`{"text":"...","history":[{"role":"user","text":"..."},{"role":"assistant","text":"..."}]}`
→ `{"reply":"...","utterance":"...","tools":[...]}`. `history` is optional and
is the prior spoken turns in this Voice sheet, not including the current
`text`. The Proxy keeps at most the last 12 user/assistant turns.

The LLM receives the bundled voice skill (`skills/treer-voice/SKILL.md`) plus a
live workspace roster and that spoken history. It may call a `treer` tool whose argv matches
`status`, `whoami`, `machine list`, and `agent list|show|prompt|read`. Those
commands run in-process under the signed-in user. The phone then speaks the
returned `reply` with system TTS (`zh-CN` when the text contains CJK, otherwise
`en-US`). Vendor LLM keys never leave the Proxy. This is not the later Voice
Live confirm-card protocol.

## Out of scope on the phone

Network editors, Audit, `/admin`, launch-profile editors, Members, recipe
install, Managed App create, a human Core Message inbox, and Voice Live
confirm-card execution. Voice command may prompt an existing Agent; it does not
create, stop, or delete from the phone.
