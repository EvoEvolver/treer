# One Agent launcher (2026-10-12)

Decision: one optional launcher, one AIS HTTP server and browser UI, small
`HarnessRuntime` adapters. `apps/` contains only workspace Apps. The user approved
feature removal in favor of a smaller design; no separate compatibility runtime
or second AIS server will survive this migration.

| Existing directory | Disposition / evidence | Final location |
| --- | --- | --- |
| `apps/agent-ui` | Git move; shared server, UI, files, queue and session state | `launchers/agent` |
| `apps/codex-ais` | Replaced by existing native app-server adapter | `launchers/agent/src/harness/codex.ts` |
| `apps/claude-ais` | Replaced by existing native stream-json adapter, including background tasks | `launchers/agent/src/harness/claude.ts` |
| `apps/cursor-ais` | Already uses `cursor-agent acp`; shared ACP adapter | `launchers/agent/src/harness/acp.ts` + profile |
| `apps/grok-ais` | Already uses `grok agent stdio`; shared ACP adapter | Same ACP adapter + profile |
| `apps/opencode-ais` | HTTP polling replaced: [upstream supports `opencode acp`](https://opencode.ai/docs/acp/) with native tools and configuration | Same ACP adapter + profile |
| `apps/dsh-ais` | Delete. [Upstream ACP](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/acp/acp/README.md) uses `session/resume`, lacks transcript replay and rejects `session/load`; not a drop-in for our adapter. No new native HTTP/SDK adapter | None; users can supply their own runtime/profile |
| `apps/pi-ui` | Delete. [Pi RPC](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/rpc.md) could support an adapter, but replacing the existing in-process extension needs a new integration, not a small merge | None; future adapter would belong under `launchers/agent/src/harness/` |
| `apps/ais-kit` | Delete; shared server/queue/history already live in Agent UI | `launchers/agent/src/` replaces it |
| `apps/codex-ui` | Delete old compatibility wrapper | None |
| `launchers/acp` | Delete Rust runtime, installers, built-in/Remote Codex UI; reuse installation ideas, not its second server | `launchers/agent/{install.sh,profiles.json,scripts/}` |
| `apps/tower` and other Managed Apps | Keep; independent workspace services | Unchanged `apps/` |

Explicit removals: Pi terminal/browser co-control, Pi compact/fork controls and UI;
DSH HTTP/SDK integration; Remote Codex presentation and compatibility endpoints;
Rust ACP writable file API, client filesystem/terminal execution, provider-specific
legacy model/effort projections, usage/cost accounting, journal and automatic
fresh-session handoff. Shared ACP keeps native tools, current config options and
load-session support; unsupported capabilities fail clearly. Native Codex/Claude
resume keeps session IDs and does not fall back to a fresh conversation. The
deprecated Codex ACP writer guard disappears with its installer: bundled Codex
profiles use native app-server, not the obsolete embedded writer.

TOWER capture is removed, including `TOWER_URL`/`TOWER_TOKEN`, canonical frame
hashes, SQLite spool, batching and restart-safe retries (`runtime/src/tower.rs`
was substantially more than a small forwarding hook). The TOWER App and its
stored data remain. Old AIS environment aliases and automatic reuse of Codex
credentials for unrelated providers are removed; provider authentication belongs
to each provider's own setup. Headless AIS presentation is replaced by the shared
embedded UI; ordinary CLI-only Agents still use user-owned profiles.

Installation will sparse-checkout a full immutable commit, run `npm ci --omit=dev`,
atomically switch `current`, save profiles, and verify them through Treer. Runtime
startup does not install/update. Tests use isolated local-repository fixtures.
New profiles: native Codex/Claude plus ACP Gemini/Grok/Cursor/OpenCode, one each.
After creating/verifying replacements, delete only the three managed legacy
profiles: **Claude ACP + Built-in UI**, **Codex ACP + Built-in UI**, **Codex ACP +
TOWER**. Keep user-owned **Codex ACP -> agent-launch codex** and **codex -> codex
--yolo** untouched.

Migration: prepare dependencies before the main merge; temporarily retain only
forwarding scripts at `apps/agent-ui/scripts/` for saved startup compatibility.
Update/show all six local Agents' startup commands, preserving cwd/session/args;
remove wrappers only after no saved local startup/profile refers to them. No
Agent, Host or Controller restart, deployment, push or Mariana operation. Final
`apps/` has no Agent adapter, interface, AIS or compatibility directory. Sources
above were inspected on 2026-10-12; absent local provider binaries mean upstream
ACP availability is documented evidence, not a live provider compatibility test.
