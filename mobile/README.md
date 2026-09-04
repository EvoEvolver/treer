# Treer native clients

iOS (`mobile/ios`) and Android (`mobile/android`) are Proxy clients. They are
not workspace Apps. Thread rendering loads bundled `mobile/agent-ui/` (the
`codex-agent-ui` web UI) in a WebView. Fleet navigation, create/confirm,
settings, inbox, and the emergency TUI are native.

Product contract: `docs/mobile.md`. Screen copy: `docs/research/2026-08-30-mobile-app-plan.md`.

## Rebuild the Agent UI bundle

```sh
just mobile-bundle-ui
```

Copies `~/dev/codex-agent-ui/apps/web/dist` into `mobile/agent-ui/`. That
directory is gitignored; rebuild it before an iOS or Android build.

## Native client header

Every login/register request:

```
X-Treer-Client: mobile_ios   # or mobile_android
```

JSON body includes `device_id` (UUID) and `device_name`. Response JSON includes
`token` only with that header. Subsequent REST and WebSocket use
`Authorization: Bearer <token>`. Never put the token in a query string.

## API used by v1

| Method | Path |
| --- | --- |
| GET | `/api/health` |
| GET | `/api/auth/config` |
| POST | `/api/auth/login` |
| POST | `/api/auth/register` |
| POST | `/api/auth/request-password-reset` |
| POST | `/api/auth/reset-password` |
| GET | `/api/auth/me` |
| PATCH | `/api/auth/profile` |
| POST | `/api/auth/logout` |
| GET | `/api/organizations` |
| GET | `/api/workspaces?organization_id=` |
| POST | `/api/workspaces` |
| GET | `/api/workspaces/{id}/snapshot` |
| POST | `/api/workspaces/{id}/bootstrap` |
| GET WS | `/api/workspaces/{id}/events` |
| GET | `/api/workspaces/{id}/voice/asr` |
| GET WS | `/api/workspaces/{id}/voice/asr/stream` |
| GET | `/api/workspaces/{id}/voice/command` |
| POST | `/api/workspaces/{id}/voice/command` (`text` plus optional spoken `history`) |
| GET | `/api/workspaces/{id}/launch-profiles` |
| POST | `/api/workspaces/{id}/launch-profiles/{id}/launch` |
| POST | `/api/workspaces/{id}/agents` |
| GET | `/api/workspaces/{id}/agents/{id}` |
| POST | `/api/workspaces/{id}/agents/{id}/prompt` |
| GET | `/api/workspaces/{id}/agents/{id}/transcript` |
| POST | `/api/workspaces/{id}/agents/{id}/stop` |
| POST | `/api/workspaces/{id}/agents/{id}/abort` |
| GET WS | `/api/workspaces/{id}/agents/{id}/terminal` |
| ANY | `/api/workspaces/{id}/agents/{id}/interface/ui/` |

WebView cookie: store `treer_session=<token>` for the Proxy host, then load
bundled `agent-ui/index.html?agent=<agent_id>` and forward relative HTTP/WS to
the interface UI tunnel.

## Build

```sh
just mobile-ios-ci
just mobile-android-ci
```
