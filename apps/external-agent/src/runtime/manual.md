# External Agent App

Lets programs outside Treer (Claude Code, Codex, scripts, CI) act as this App's
Treer Agent with an App-issued bearer token, under the App's workspace Policy.
Browsers and `Accept: text/html` receive the owner settings page.
`GET /SKILL.md` is the guide to hand to an external program.

## External programs (data port)

| Route | Use |
| --- | --- |
| `GET /health` | Liveness, no credentials |
| `POST /v1/device/request`, `POST /v1/device/token` | Device-code onboarding, no credentials |
| `POST /v1/cli` | Run the real `treer` CLI: `{"argv":[...],"stdin":"","timeout_ms":30000}` |
| `GET /v1/inbox?limit=50[&cursor=]`, `POST /v1/inbox/ack` | Registered agent key's own inbox |
| `GET/POST /v1/external-agents`, `POST /v1/external-agents/revoke-token` | Full keys register agents / revoke keys they created |

All `/v1` calls except device onboarding need exactly one
`Authorization: Bearer ext1_...` header and refuse browser Origin, Fetch
Metadata, cookies and `X-Treer-*` headers. Responses are
`{"request_id":...,"result":...}`.

## Management (owner port)

Workspace owners (via the Console) and workspace Agents use the same API. An
Agent calls `http://APP_NAME.treer/owner/...` through its runtime proxy; Core
only issues it an identity for this service if its Policy allows
`identity.token.issue` on the service. This App's own execution identity is
refused. Human browser mutations also need same-origin `Origin`,
`Sec-Fetch-Site` and `X-External-Agent-CSRF: 1`.

```sh
curl http://APP_NAME.treer/owner/state
curl -X POST -H 'Content-Type: application/json' http://APP_NAME.treer/owner/tokens \
  -d '{"name":"ci","days":30,"authority_confirmed":true}'
```

| Route | Body / result |
| --- | --- |
| `GET /owner/state` | Identity, enabled, generation, tokens, `readiness` (inbox/CLI) |
| `GET /owner/audit` | Last 100 body-free execution records |
| `GET /owner/integration` | Data URL, blockers, copyable guide |
| `POST /owner/tokens` | `{name,days:30|90,authority_confirmed:true}`, plaintext once |
| `POST /owner/revoke` | `{id}` |
| `POST /owner/activation` | `{enabled,generation,agent,policy_reviewed}` |
| `GET/POST /owner/external-agents`, `POST /owner/external-agents/tokens` | `{key,name,days,authority_confirmed}` |
| `GET /owner/device/requests` | Pending/approved device requests |
| `POST /owner/device/lookup` | `{user_code}` |
| `POST /owner/device/decision` | `{user_code,approve,days,authority_confirmed}`; **human owner only** |

The external API starts disabled. Tokens expire after 30 or 90 days, are shown
once and only a verifier is stored. Errors use `error.code`
(`authentication_required`, `disabled`, `capacity_unavailable`,
`invalid_request`, `inbox_unsupported`, `cli_unavailable`, `outcome_unknown`).
