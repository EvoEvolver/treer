# Console embed acceptance (real browser, real launcher-backed agent)

> **Not part of the launcher payload.** Nothing in this directory is bundled into the launcher or its native artifacts; these are
> test and release tooling that run from a checkout against a canary. The script writes a report file (default
> `./console-embed-acceptance-report.json` in the working directory); that name
> is git-ignored because a report can name canary hosts, workspace paths and agent ids. Keep reports under `.treer/` or set
> `ACCEPT_REPORT_FILE` to a path outside the repository.

### Pinned scripts

| Script | sha256 (as pinned by release) |
| --- | --- |
| `console-embed-acceptance.mjs` | `6b19f4a30e7c10cc246a2fda138e61337cba302a67782985ae4783ffa9d3ba40` |

An acceptance record names the hash the run printed. Editing the script changes its hash: the new hash needs a review and a
new pin before it may be used as acceptance evidence (the release runner refuses any other hash).

Team rule (#98, 2026-10-19): any change to a Console, browser or embedding path must pass a **real-browser acceptance on
canary against a real launcher-backed agent** before it ships. `console-embed-acceptance.mjs` is the launcher/UI side of that
check. Release owns where it runs (canary-a, one disposable fake-harness agent, no real credentials) and the member session.

It changes nothing: it opens the Console in real Chromium, Firefox and WebKit as the test member and reports a pass/fail per
step.

## Run

All inputs come from the environment (no argv, so nothing secret appears in a process list):

| Variable | Meaning |
| --- | --- |
| `ACCEPT_CONSOLE_URL` | Console origin, e.g. `https://console.example` |
| `ACCEPT_WORKSPACE_PATH` | `/orgs/<org>/workspaces/<workspace>` |
| `ACCEPT_AGENT_ID` | the disposable launcher agent's workload id |
| `ACCEPT_STORAGE_STATE` | path of a Playwright storage-state file for the test member (read only, never printed or copied) |
| `ACCEPT_ENGINES` | optional, default `chromium,firefox,webkit` |
| `ACCEPT_REPORT_FILE` | optional, default `./console-embed-acceptance-report.json` (mode 0600) |
| `ACCEPT_ALLOWED_HOST_SUFFIXES` | **required**; comma list of canary hosts or canary-only suffixes the run may contact, e.g. `app.canary.example,proxy.canary.example,canary.apps.example`. Nothing is guessed from the Console host; an entry with fewer than three labels (a bare registrable domain) is refused, and the Console host must be covered, otherwise exit 2 before any browser starts |
| `ACCEPT_IGNORED_BLOCKED_HOSTS` | optional; hosts whose requests are aborted (never contacted) and counted in the report instead of failing the run, e.g. `static.cloudflareinsights.com` (an edge beacon injected into the Console); empty by default |
| `ACCEPT_EXPECT_COMPATIBILITY` | set to `1` only to accept a non-isolated embedded view (Console or Proxy origin); by default the embedded view **must be isolated**, see step 2 |
| `ACCEPT_ISOLATED_HOST_REGEX`, `ACCEPT_ALLOW_HTTP` | defaults `^ag-` and https; relaxed only by the local self-test |
| `ACCEPT_TIMEOUT_MS` | optional per-step wait, default 30000 |

```bash
cd launchers/agent && npm ci        # @playwright/test and the three browsers (npx playwright install chromium firefox webkit)
ACCEPT_CONSOLE_URL=... ACCEPT_WORKSPACE_PATH=... ACCEPT_AGENT_ID=... ACCEPT_STORAGE_STATE=... \
  node tests/acceptance/console-embed-acceptance.mjs
```

Exit 0 only if every engine passes every step. The script prints its own SHA-256 first and writes it into the report;
release should pin that hash.

## Steps (per engine)

1. The Console opens the agent tab (an `iframe[title$=" interface"]`).
2. The embedded view reaches the Agent UI and is **isolated**: the frame is served over https from an `ag-*` host under an allowed suffix, not from the Console origin and not from the Proxy tunnel origin.
3. First paint: the conversation header is visible and there is no error banner.
4. Sending a message works through the embed path (HTTP 200 on `/api/prompt`, composer clears). The transcript is not asserted
   because a fixture harness may never echo the turn.
5. The Files panel opens and shows the file tree.
6. The Console cannot read the framed Agent document (cross-origin).
7. The standalone Agent page loads for the member.
8. Negative: a foreign page, in a browser holding the member's session, cannot frame the Agent origin. Header evidence is required: the Agent origin document's CSP must carry `frame-ancestors` limited to the Console origin (`'self'` tolerated), and the frame must not render. A blank frame alone does not pass.
9. Negative: the Agent origin without a session shows no composer **and** gives refusal evidence: a 401/403, or a redirect away (the authorize step). A network failure does not pass.
10. No console errors, failed requests, HTTP errors or blocked requests on the member's pages.

The report holds, per engine, the steps with pass/fail, and for every document loaded: its URL **without query or
fragment**, the status code and an allow-list of response headers (CSP, frame, COOP/COEP/CORP, referrer, nosniff,
permissions, cache-control, content-type).

## What it never does

No cookies, storage state, request headers, query strings, fragments or message bodies are recorded; console text and URLs are
redacted for token-looking runs (`iec_v1_...` and long base64/hex strings). No traces, videos, HARs or screenshots are
written. Requests to hosts outside the allow-list are aborted and fail the run. The script itself makes no network request
other than the browser's navigation to the Console and the agent origin. An app cancelling its own fetch is counted
(`ignored_aborted_fetches`), not failed; console warnings are not failures.

## Not covered (needs release or Core)

* The nonce/bootstrap handoff, the partitioned cookie, logout ending the embed session, and "a cross-origin request with the
  embed cookie is refused" are Core behaviours; this script checks their user-visible outcome only (the view renders
  isolated; foreign framing and session-less access are refused). Release should add header-level probes for the rest.
* Strict-refusal and compatibility-choice behaviour are exercised by web-dev's matrices, not by this script, because the
  canary fixture is expected to take the isolated path.
