# External Agent App

A scoped external-program entry point. Tokens act as this App's execution Agent under existing Core Policy, never as a human account. Browsers and `Accept: text/html` receive owner settings.

## Inspect

Keep your bearer header in a private curl config file (0600), not a URL, shell history, or Agent prompt. Set APP_URL to this App's configured origin.

```sh
curl --config /private/path/external-agent.curl "$APP_URL/v1/agents"
curl --config /private/path/external-agent.curl "$APP_URL/v1/agents/ag_0123456789abcdef0123456789abcdef"
```

GET suffixes `/output` and `/transcript` return bounded content authorized by Core. Output defaults to 100 lines; transcript defaults to one turn. Requests cannot choose another workspace, transport, identity or command.

## Mutate

POST suffixes `/prompt`, `/stop`, `/start`, `/restart` execute once, without retries. Prompt JSON is `{"text":"task"}`; other mutations use `{}`. Use `Content-Type: application/json`. Prompt uses current workspace Policy and always adds an untrusted-external-input marker. An outcome_unknown response may follow a committed mutation: inspect state before deciding to issue a new operation.

## Authentication and Policy

External APIs accept exactly one Authorization Bearer header, refuse browser-origin/fetch requests and cookies, and emit no CORS permission. No URL/query credentials. Tokens expire after 30/90 days. A revoked token cannot admit or dispatch another request; an already dispatched operation may finish. Each request reaches Controller and Core under the same fixed execution identity; the App does not cache or replace Policy decisions.

The anonymous data origin rejects all `/owner/` routes. Owner settings live on a separate workspace-authenticated management port with an independent connection pool; the management port rejects `/v1/`. Open settings through the App's Console entry. If management is unreachable, use the independently authorized Console/Controller stop for the App; already dispatched operations may finish.

The API starts disabled. Owner settings require fresh service-audience human-owner verification on every request, plus browser mutation protection. Public direct access does not provide that identity: open the App inside the authenticated Console. Activation requires fresh owner acknowledgement of the current App/generation and current workspace Policy; legacy evidence pins are not required. Explicitly authorized local deployment-operator confirmation is also supported without impersonating a human. All tokens share the App's authority; read-only automation needs a separate App with enforcing read-only Policy.

Audit records identify token, operation and request without storing prompts or output. Responses and secrets are no-store. Errors use `error.code`: authentication_required, disabled, capacity_unavailable, invalid_request, unavailable, or outcome_unknown. Execution responses contain a request_id and result. This App is not OS isolation from another process running under the same account.
