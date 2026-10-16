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

Login, registration and both password-reset requests send:

```
X-Treer-Client: mobile_ios   # or mobile_android
```

Login/register JSON body includes `device_id` (UUID) and `device_name`. Response JSON includes
`token` only with that header. Subsequent REST and WebSocket use
`Authorization: Bearer <token>`. Never put the token in a query string.

Password-reset calls are pre-authenticated: they do not send a Bearer session.
iOS disables automatic cookies for native pre-auth requests (including login
and registration); Android uses the existing cookie-free OkHttp client. The
native marker is not a bypass for cookie-authenticated operations or requests
with browser Origin/Fetch Metadata. Both native clients currently use password
authentication and do not navigate to provider OAuth-start GET routes.

Run `node mobile/tests/native-auth.mjs` from the repository root for the isolated
Foundation request regression (the real APIClient with only UIDevice stubbed,
all traffic intercepted by URLProtocol). The Android JVM `NativeAuthTest` uses a
loopback HTTP server on a random port, with synthetic data only; run it through
`./gradlew testDebugUnitTest` from `mobile/android` on a configured
JDK/Android SDK machine. Neither test uses real accounts or performs IdP login.

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

## WebView security limitations

Injected session cookies are host-only, HttpOnly and SameSite=Strict, with Secure
for HTTPS. HTTP development endpoints have no transport confidentiality. This
protects direct cookie reads; it does not isolate embedded JavaScript or restrict
native bridge authority. Current native bridges still need origin/frame checks,
navigation restrictions and request-path confinement before untrusted interfaces
can be supported. Android also enables remote WebView debugging, mixed content
and third-party cookies; these are remaining hardening work, not safe defaults
for untrusted content. See the [Web security review](../docs/research/2026-10-12-security-web.md).

Run `node mobile/tests/session-cookie.mjs` from the repository root for the
isolated Foundation regression (Swift toolchain required). Android's
`SessionCookieTest` covers the equivalent header builder. Neither test certifies
runtime WKWebView/Android CookieManager enforcement; device tests remain needed.
