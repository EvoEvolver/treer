# Git Gateway App

Git Gateway gives every workspace Agent, on any enrolled machine, GitHub
access without per-machine authorization. One gateway machine holds the GitHub
credential; Agents call `http://git-gateway.treer/` through their workload
proxy, the gateway verifies their Treer identity, applies its rules, injects
its credential and streams the request to GitHub. The runtime manual for
Agents is [AGENT.md](AGENT.md) (served at `/`); this README covers design and
operation.

## Design

- **A separate App, not a Gits mode.** [Gits](../gits/README.md) is an open
  local host: it trusts every workspace caller, holds no secret and has no
  rules. The gateway is a credential holder whose whole job is identity
  verification, per-caller rules and auditing in front of someone else's
  server. Folding that into Gits would give the unauthenticated host a secret
  and a second trust model in one process; the two share only the Smart HTTP
  route shape.
- **Routes.** `/github/OWNER/REPO.git/{info/refs,git-upload-pack,git-receive-pack}`
  is forwarded to `https://github.com/OWNER/REPO.git/...` with
  `Authorization: Basic x-access-token:TOKEN`; `/github-api/PATH` goes to
  `https://api.github.com/PATH` with `Authorization: Bearer TOKEN`. Request and
  response headers are whitelisted (incoming `Authorization`, `Cookie` and
  Treer headers never reach GitHub; GitHub's `WWW-Authenticate` never reaches
  the client, and an upstream 401 becomes 502, so Git never prompts).
- **Streaming.** Bodies are streamed both ways in 64 KiB pieces with
  Content-Length or chunked framing; gzip request bodies pass through
  unchanged. Pushes are capped by `max_push_bytes` (default 1 GiB),
  upload-pack requests by 256 MiB and REST bodies by 25 MiB. Redirects are
  never followed. Python 3.10+ standard library only.
- **Identity.** Every route except `/`, the UI assets and `/health` requires
  `X-Treer-Identity-Token` (or `Authorization: Bearer`), verified on each
  request with the Proxy's `/.treer/apps/identity/verify` against this App's
  service and workspace. Display names and paths come from the Core directory
  (best effort, cached 10 minutes) and are never used to authorize.
- **Push rules.** Before anything is forwarded, the gateway decodes the
  receive-pack command list (pkt-lines, also when gzip-encoded) and checks each
  ref: deleting a `protected_refs` ref is refused for everyone; humans and
  `trusted_agents` may update any other ref; other Agents only
  `agent_push_refs` (default `refs/heads/agent/**`). A denied push is drained
  and answered with a receive-pack report, so `git push` prints
  `! [remote rejected] ... (reason)`; nothing reaches GitHub. Git's lone-flush
  auth probe before a large push is forwarded unchanged.
- **REST rules.** Reads of allowlisted repositories and `api_global_reads`;
  Agent writes only through `agent_api_writes` (issues, PRs, reviews, re-runs,
  deleting `heads/agent/**`); humans and `trusted_agents` may write anything
  under allowlisted repositories. GraphQL is not forwarded.
- **Audit.** Every fetch, push (each ref `old→new` with GitHub's per-ref
  report), REST call, denial, rule change and GitHub App event is appended to
  `audit.jsonl` (rotated once at 8 MiB to `audit.1.jsonl`).

## Credentials

`credential_mode` selects the source: `auto` (default: the GitHub App when it
is installed for the repository owner, otherwise `gh`), `gh` or `github_app`.

1. **`gh` (default, zero clicks).** The gateway runs
   `gh auth token --hostname github.com` on its own machine, caches the token,
   re-reads it every `GATEWAY_GH_REFRESH_SECONDS` (600) and immediately after
   GitHub answers 401 (the Git advertisement and REST calls are retried once).
   Everything is done as the `gh` account (on mac_m2 `doomspec`, scopes
   `repo`, `workflow`), so GitHub shows that account as the pusher; commits keep
   each Agent's own author identity.
2. **GitHub App (optional, two owner clicks).** In the UI, **Create GitHub
   App…** prepares a manifest (contents, pull requests, issues, actions and
   workflows write; checks and statuses read; webhook off) and **Continue on
   GitHub** posts it to `github.com/settings/apps/new` (or
   `/organizations/ORG/...`). GitHub redirects back to
   `api/github-app/callback`; the gateway checks the single-use, 30-minute
   state bound to the same signed-in human, exchanges the code
   (`POST /app-manifests/{code}/conversions`) and stores the App ID and
   metadata in `github-app.json` and the private key in `github-app.pem`
   (both 0600; the client and webhook secrets are discarded). The owner then
   follows the **Install** link and selects the repositories. The gateway
   signs App JWTs with RS256 in pure Python (PKCS#1 v1.5 over the PKCS#1 or
   PKCS#8 key; checked against `openssl` in tests), lists installations and
   mints installation tokens (`POST /app/installations/{id}/access_tokens`),
   cached until five minutes before expiry. Pushes then appear as the App's
   bot. The callback needs a browser URL GitHub can redirect to: the App's
   Workspace ingress origin or the Console tunnel URL (discovered or
   `GATEWAY_PUBLIC_URL`).

## Deploy on mac_m2

Run it as a Managed App on the machine whose `gh` is logged in. It needs no
`--public`; Agents use the service name and humans use the Console.

```sh
treer app create --machine mac_m2 --folder /infra --name git-gateway --cwd treer --port 9450 \
  env -- \
  GATEWAY_LISTEN=127.0.0.1:9450 \
  GATEWAY_DATA_DIR=/Users/dooms/.local/share/treer/apps/git-gateway \
  GATEWAY_GH_BIN="$(command -v gh)" \
  GATEWAY_SEED_TRUSTED_AGENTS=LEADER_AGENT_ID \
  python3 apps/git-gateway/gateway.py
treer app show /infra/git-gateway
curl -fsS http://git-gateway.treer/api/status | jq .credential   # from any managed Agent
```

The App discovers its service ID, workspace and browser URL with
`treer app list` (its own `TREER_AGENT_ID`); `TREER_PROXY_URL` is inherited.
If the service name is taken Treer adds a suffix: use the name from
`treer service list` in `GATEWAY_AGENT_URL`. `GATEWAY_SEED_*` apply only when
`config.json` does not exist yet; afterwards change rules with the UI or
`PUT /api/config`. `treer app restart /infra/git-gateway` loads new code with
the same identity and URL.

| Variable / flag | Default | Meaning |
| --- | --- | --- |
| `GATEWAY_LISTEN` / `--listen` | `127.0.0.1:9450` | Listener; match `--port` |
| `GATEWAY_DATA_DIR` / `--data-dir` | `.treer/apps/git-gateway` | Private state (0700) |
| `GATEWAY_PROXY_URL` / `--proxy-url` | `$TREER_PROXY_URL` | Proxy used for identity verification |
| `GATEWAY_SERVICE_ID`, `GATEWAY_WORKSPACE_ID` | discovered | Identity audience (set for external supervision) |
| `GATEWAY_PUBLIC_URL` | discovered | Browser URL; GitHub App redirect target |
| `GATEWAY_AGENT_URL` | `http://git-gateway.treer` | Address shown to Agents |
| `GATEWAY_GH_BIN`, `GATEWAY_GH_REFRESH_SECONDS` | `gh`, `600` | `gh` mode |
| `GATEWAY_GITHUB_URL`, `GATEWAY_GITHUB_API_URL` | github.com, api.github.com | Upstreams (tests use a fake) |
| `GATEWAY_SEED_REPOS`, `GATEWAY_SEED_TRUSTED_AGENTS` | — | Comma lists for the first `config.json` |
| `TREER_BIN` | `treer` | CLI used for discovery |

## State, backup and trust

`GATEWAY_DATA_DIR` holds `config.json`, `audit.jsonl`, and in GitHub App mode
`github-app.json` and `github-app.pem`, all 0600. The `gh` token is never
written by the gateway. Back the directory up encrypted; the PEM is a GitHub
credential. Same-UID processes on the gateway machine can read `gh`'s token
and the PEM anyway, so the rules protect against Agents elsewhere, not
against code on the gateway machine. Any workspace Agent that can reach the
service can read every allowlisted repository and push `agent/**`; narrow
access further with Treer Policy on the service. Keep the App
workspace-authenticated (never `--public`).

## Test

```sh
python3 -m unittest discover -s apps/git-gateway/tests -p 'test_*.py' -v
python3 apps/git-gateway/tests/smoke_real.py   # real GitHub via local gh; pushes and deletes agent/gateway-smoke-<ts>
```

The unit suite runs a fake GitHub (real `git http-backend` over bare
repositories, chunked responses, credential checks and REST/App endpoints), a
fake Treer Proxy verifier and a fake `gh`: clone/fetch/push, chunked 3 MiB
pushes and the size cap, branch rules, protected deletes, allowlist, identity
rejection, REST rules and Link rewriting, audit entries, the GitHub App
manifest flow, installation tokens, RS256 against `openssl` and `gh` refresh.
