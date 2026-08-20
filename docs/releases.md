# Release process

- Status: maintained
- Last reviewed: 2026-08-19

Treer promotes an explicit Git commit through Canary before Production. A
release is not a branch name, a mutable `latest` label, or the contents of an
operator's current directory. The ignored `.treer/releases/` directory stores
the local release manifest and the exact frontend artifact that passed Canary.

## Platform boundary

Railway runs the Proxy replicas, PostgreSQL, NATS, and the two-machine Canary
test services. Cloudflare Workers Static Assets runs the independent React App:

| Environment | App | Proxy |
| --- | --- | --- |
| Canary | `https://app.canary.treer.ai/` | `https://proxy.canary.treer.ai/` |
| Production | `https://app.treer.ai/` | `https://proxy.treer.ai/` |

The App Worker serves Vite output directly from Cloudflare. Only `/health` and
`/config.json` invoke Worker code. Runtime configuration keeps one frontend
bundle independent of the Proxy origin.

## Canary release

The operator needs authenticated Railway and Wrangler CLIs, Docker, `just`,
`pnpm`, `jq`, and `curl`. Start from a clean checkout of the candidate commit:

```bash
just release-canary HEAD
```

The command refuses a dirty worktree, a revision other than `HEAD`, or a commit
not contained in `origin/main`. It runs the complete local gate, stores the
built `index.html` by SHA-256, deploys the Canary Proxy and App, and runs the
black-box two-machine test. Success produces:

```text
.treer/releases/<full-commit>/manifest.json
```

The manifest records the commit, frontend checksum, Railway deployment ID,
Cloudflare Worker version ID, endpoints, and test timestamp. Failure never
creates an eligible manifest.

## Production promotion

Check out the same clean commit and retain its `.treer/releases` directory,
then run:

```bash
just promote-production .treer/releases/<full-commit>/manifest.json
```

Promotion requires `status: canary_passed`, verifies the commit and artifact
checksum, deploys the Production Proxy and the exact Canary frontend artifact,
then checks Proxy health, App health/configuration, and wildcard TLS routing.
It updates the manifest to `status: production_deployed` with Production IDs.

Only one release may run in a checkout at a time. The scripts use
`.treer/release.lock` to reject overlapping operators.
Self-hosted forks can set `TREER_RELEASE_REMOTE_REF` to their protected release
branch.

## Compatibility and rollback

- Add Proxy API fields before the App begins using them. Stop App use before a
  later release removes an API.
- Use expand-first PostgreSQL changes so old and new Proxy replicas can overlap.
- Before the first stable release, a Controller protocol bump may deliberately
  require a coordinated Proxy rollout and machine re-enrollment. Record that
  boundary in the release notes and reset Canary as one unit. Once stable
  releases begin, keep the current and previous Controller protocols usable
  during machine rollout.
- Roll back the App with `wrangler rollback --env production <version-id>`.
- Roll back the Proxy to its prior Railway deployment. Database contraction is
  a separate release and is never part of an automatic code rollback.

The first implementation rebuilds the Proxy from the same commit in each
Railway environment. Moving to a single OCI image digest is a future hardening
step; the frontend artifact is already byte-identical across promotion.
