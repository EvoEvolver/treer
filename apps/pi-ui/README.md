# Pi UI

Pi UI is a small browser interface for a single Pi Agent. Pi loads it as an
extension, so the terminal and browser control the same Pi process and session.
The extension listens only on the Agent's private loopback interface and
registers itself as a `treer.agent-interface/v1` server. Its manifest exposes
the browser UI alongside prompt and transcript capabilities.

## Start an Agent

Run from a checkout visible to the selected machine. `--cwd` is relative to
that machine's configured root; this example assumes the checkout is `treer`:

```bash
treer agent admin create --machine self --kind shell --name pi-ui --cwd treer -- \
  pi --extension apps/pi-ui/extension.mjs --approve
```

For repeated launches, save it as a workspace Agent profile. Using port `0`
lets each Agent choose a free loopback port, including on hosts where Agents do
not have separate network namespaces:

```bash
treer agent admin profile create "Pi + UI" \
  --description "Pi coding agent with the Treer embedded browser UI" \
  --cwd treer env -- \
  PI_UI_PORT=0 pi --extension apps/pi-ui/extension.mjs --approve
```

The profile then appears in Treer's Create Agent dialog and can also be launched
from the CLI:

```bash
treer agent admin profile launch "Pi + UI" --name pi-ui
```

When launching on a different machine, pass its registered name or ID with
`--machine`.

The extension defaults to `127.0.0.1:4180`. Set `PI_UI_PORT` before launching
Pi to use another port, or use `0` to select a free port automatically. Set
`PI_UI_AUTO_REGISTER=0` to run the HTTP interface without registering it with
Treer.

The AIS routes are `GET /v1/manifest`, `/v1/health`, `/v1/status`,
`/v1/transcript`, and `/v1/events`, plus `POST /v1/prompts` and `/v1/abort`.
`/v1/transcript` pages by conversation turn: `page` (or `cursor`) is the
0-based turn index, and `limit` is the number of turns (default 1).
Transcript paging is implemented with the shared [`ais-kit`](../ais-kit/README.md).
Prompt requests are deduplicated by Treer's `operation_id`. The extension
registers once at startup; the Controller revalidates its local registration
cache when recovering a running Pi session after a hot restart.

No npm install or frontend build is required to run the checked-in App. Pi,
Node.js, and the `treer` CLI must be available in the Agent environment. Run
`npm install && npm run build` in this directory only when updating the bundled
Markdown renderer.

## Scope

The interface intentionally owns one Pi session. It provides the conversation
timeline, live tool activity, prompt/steer/follow-up delivery, abort, and
compaction. Fork creates a sibling Treer Agent on the same machine and working
directory with a cloned Pi session; it requires the parent Agent's Policy to
allow Agent creation. Message content is rendered as sanitized GFM Markdown.
Provider setup, worktrees, terminals, multi-session catalogs,
notifications, updates, and Agent orchestration remain with Pi, Treer, or the
operator instead of being duplicated here.

The timeline and composer interaction model are adapted from
[`pi-gui`](https://github.com/minghinmatthewlam/pi-gui) at commit
`eb9a7380705dffad36db3efa771ee825aafbef6f`, used under its MIT license. This
App does not copy pi-gui's Electron runtime, SDK supervisor, worktree manager,
PTY integration, provider settings, or release infrastructure. See
[`LICENSE.pi-gui`](LICENSE.pi-gui).

The checked-in Markdown bundle contains Marked and DOMPurify. Their license
texts are included as [`LICENSE.marked`](LICENSE.marked) and
[`LICENSE.dompurify`](LICENSE.dompurify).

## Test

```bash
node --test apps/pi-ui/*.test.mjs
```
