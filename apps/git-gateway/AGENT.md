# Git Gateway

Git Gateway lets every workspace Agent, on any enrolled machine, clone, fetch
and push GitHub repositories and call the GitHub REST API without its own
GitHub login. The gateway holds the only GitHub credential, checks its rules,
injects the credential and forwards the request to GitHub. Browsers and
`Accept: text/html` receive the human interface from this same URL.

Use the address `http://git-gateway.treer`. Managed Agents already have
`http_proxy` set to their Treer workload proxy, which injects your identity on
every request to a `*.treer` host. You need no token, `gh auth login`,
credential helper or `git config http.proxy` (the environment variable is
enough). If `echo $http_proxy` is empty you are not a managed workload; the
gateway then answers `401 authentication_required`.

## Inspect

```sh
curl -fsS http://git-gateway.treer/api/status | jq '{credential: .credential.active, repos, rate_limit}'
curl -fsS http://git-gateway.treer/api/config | jq .config
curl -fsS 'http://git-gateway.treer/api/audit?limit=20' | jq '.entries[] | {at, op, repo, result, refs}'
git ls-remote http://git-gateway.treer/github/EvoEvolver/treer.git
```

## Clone and fetch

```sh
git clone http://git-gateway.treer/github/EvoEvolver/treer.git
git clone --depth 1 http://git-gateway.treer/github/EvoEvolver/treer.git   # shallow
```

For an existing checkout, add the gateway as a remote (or replace `origin`):

```sh
git remote add gw http://git-gateway.treer/github/EvoEvolver/treer.git
git fetch gw
```

Only repositories in the gateway's `repos` allowlist are reachable
(`403 repo_not_allowed` otherwise).

## Push (mutation)

Agents may push only refs matching `agent_push_refs`, by default
`refs/heads/agent/**`. Name your branch after your job:

```sh
git push origin HEAD:agent/my-job
git push origin --delete agent/my-job       # deleting your agent/* branch is allowed
```

Pushing `main` or any other ref fails with
`! [remote rejected] main -> main (gateway: Agents may push only refs/heads/agent/**)`
and nothing reaches GitHub. A push that mixes allowed and denied refs is
rejected as a whole. Nobody, not even a human, may delete a `protected_refs`
ref through the gateway. Humans and Agents listed in `trusted_agents` may push
any other ref. Commits keep your own `user.name`/`user.email`; the gateway
changes only the transport credential, so GitHub shows the gateway account as
the pusher. Pushes above `max_push_bytes` fail with `413`.

Open a pull request for review instead of asking for `main` access:

```sh
curl -fsS -X POST http://git-gateway.treer/github-api/repos/EvoEvolver/treer/pulls \
  -H 'Content-Type: application/json' \
  -d '{"title":"My change","head":"agent/my-job","base":"main","body":"What and why"}' | jq .html_url
```

## GitHub REST API

`http://git-gateway.treer/github-api/PATH` forwards to
`https://api.github.com/PATH` with the gateway credential. Any `Authorization`
or `Cookie` you send is dropped. Read CI status for your branch:

```sh
api=http://git-gateway.treer/github-api
sha=$(git rev-parse HEAD)
curl -fsS "$api/repos/EvoEvolver/treer/commits/$sha/status" | jq '{state, statuses: [.statuses[] | {context, state}]}'
curl -fsS "$api/repos/EvoEvolver/treer/commits/$sha/check-runs" | jq '.check_runs[] | {name, status, conclusion}'
curl -fsS "$api/repos/EvoEvolver/treer/actions/runs?branch=agent/my-job&per_page=5" | jq '.workflow_runs[] | {name, status, conclusion, html_url}'
curl -fsS "$api/repos/EvoEvolver/treer/branches/agent/my-job" | jq .commit.sha
```

Rules for the REST passthrough:

- `GET`/`HEAD` on `/repos/OWNER/REPO/...` for allowlisted repositories, plus
  the global reads in `api_global_reads` (`/rate_limit`, `/user`, `/search/*`).
- Agent writes only match `agent_api_writes`: create/edit issues, comments and
  labels; create/edit pull requests, reviews and review requests; re-run
  workflow runs; delete `heads/agent/**` refs. Merging pull requests, changing
  repository settings or moving other refs needs a human or a trusted Agent.
- GraphQL (`/graphql`) is not forwarded. Pagination `Link` headers are
  rewritten to the gateway; URLs inside JSON bodies still name
  `api.github.com`, so replace that prefix with `$api` before following them.

## Authentication, rules and trust

- Every call except `/` and `/health` must carry the Treer identity that the
  workload proxy injects (`X-Treer-Identity-Token`). The gateway verifies it
  with the Treer Proxy for this App's service on every request and records your
  Agent ID, name and machine in the audit log.
- Every fetch, push (with each ref `old→new` and GitHub's per-ref result), REST
  call, denial and rule change is audited.
- Rule changes (`PUT /api/config`) are limited to humans and `trusted_agents`.
- The gateway credential can do whatever its GitHub account or App can. The
  rules here are the guard; do not try to work around them, and never push
  credentials into a repository.

## API

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/health` | Liveness (no identity) |
| `GET` | `/api/status` | Credential mode, token expiry, rate limit, caller |
| `GET` | `/api/config` | Rules |
| `PUT` | `/api/config` | Change rules (partial object; humans and `trusted_agents`) |
| `GET` | `/api/audit?limit=&repo=&op=&result=&caller=&kind=` | Newest audit entries first |
| `POST` | `/api/credentials/refresh` | Re-read `gh` / re-list App installations |
| `POST` | `/api/github-app/manifest` | Start GitHub App creation (human browser only) |
| `GET` | `/api/github-app/callback` | GitHub App manifest redirect target |
| `DELETE` | `/api/github-app` | Forget the stored GitHub App |
| `GET`/`POST` | `/github/OWNER/REPO.git/{info/refs,git-upload-pack,git-receive-pack}` | Git Smart HTTP |
| any | `/github-api/PATH` | GitHub REST passthrough |

Config fields: `repos` (`owner/name` or `owner/*`), `agent_push_refs`,
`protected_refs`, `trusted_agents` (Agent IDs), `credential_mode`
(`auto`, `gh`, `github_app`), `max_push_bytes`, `api_global_reads`,
`agent_api_writes` (`"METHOD /repos/{repo}/path"`). In patterns `*` matches
within one path segment and `**` across segments. Errors use
`{"error":{"code":"...","message":"..."}}`; Git routes return a plain-text
`gateway: ...` message.
