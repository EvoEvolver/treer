# Provider installation data contract (Issue #123)

This is the proposed data handoff for Core's approved provider-cli route contract.
The routes and installation are Core-owned; this document does not enable an
installation command. The serialized table will live at
`launchers/agent/provider-recipes.json` and enter the prepared payload's existing
file/hash manifest. Core embeds those release bytes, never reads an installed or
user-editable table, and validates before expanding any command.

## Serialized table

Top-level fields: `schema: "treer.provider-recipes/v1"`, `schema_version: 1`,
`recipes: []`. Each entry has `provider_id`, `recipe_id`, positive integer
`recipe_version`, `platforms` (explicit members of the five release platforms),
`binary_names` (exact bare names), `supported`, `official_docs`, and `login_hint`
(plain English text, no login execution). Unsupported entries have a fixed
`unsupported_reason` and **no** `install` or `check` command. Supported entries
have `node_min: 22`, `install` and `check`:

```json
{
  "provider_id": "codex",
  "recipe_id": "codex-npm-posix",
  "recipe_version": 1,
  "platforms": ["darwin-aarch64", "darwin-x86_64", "linux-aarch64", "linux-x86_64"],
  "binary_names": ["codex"],
  "supported": true,
  "node_min": 22,
  "install": {
    "kind": "npm-global",
    "package": "@openai/codex",
    "version": "0.163.0",
    "registry": "https://registry.npmjs.org/",
    "ignore_scripts": true
  },
  "check": {"executable": "codex", "args": ["--version"]},
  "official_docs": "https://github.com/openai/codex",
  "login_hint": "Run codex in your own terminal to sign in. Treer does not handle your provider credentials."
}
```

`install` is typed package data rather than arbitrary executable/argv input.
Core owns **one** expansion function, used both for the response's `display`
and for execution. POSIX expansion is a vetted npm executable with literal
`install --global --registry=https://registry.npmjs.org/` plus the exact
`package@version` and `--ignore-scripts` when declared. No sudo, shell, editable
argv, environment substitutions, URL scripts, version tags or ranges.

On Windows Core must resolve trusted absolute Node and its adjacent
`node_modules/npm/bin/npm-cli.js`, verify a regular physical file inside that
Node distribution (including junction/long-path handling), and directly invoke
Node with that entry and the same literal npm arguments. Never execute npm.cmd,
cmd.exe or PowerShell command strings. The machine-scoped display must contain
the **actual expanded** executable/arguments, not symbolic placeholders. Until
Core confirms this strategy and tests it, Windows entries are unsupported;
they must not silently fall back to shell execution. npm itself may execute
package lifecycle scripts when the recipe explicitly allows them; this is
third-party code execution, not a shell-free dependency sandbox.

`table_hash` is SHA-256 of the exact embedded UTF-8 table bytes.
`recipe_hash` is SHA-256 of compact UTF-8 JSON, recursively sorting object keys
lexicographically and retaining array order, excluding only the top-level
`recipe_hash` field. The on-disk table need not store hashes: Core computes them
after strict validation and returns them with recipe/table metadata. Golden
hash fixtures will be shared with Core. Unknown provider/platform, unsupported
entry, extra field, malformed package/version/registry or a hash mismatch fails
closed; none can prepare an installation.

## Source and package review, 2026-10-16

Only official documentation and metadata from registry.npmjs.org were read;
no package or provider installer was executed and no credentials were read.
These are candidate pins for review, not assertions of runtime compatibility:

| Provider | Candidate | Lifecycle / disposition | Official source |
| --- | --- | --- | --- |
| Codex | `@openai/codex@0.163.0` | Published metadata has no scripts; propose ignore-scripts. | [OpenAI repository](https://github.com/openai/codex) |
| Claude Code | `@anthropic-ai/claude-code@2.1.302` | `postinstall: node install.cjs` links the native optional dependency. Requires lifecycle review; ignore-scripts would break this install. | [Claude setup](https://code.claude.com/docs/en/setup#install-with-npm) |
| Gemini | `@google/gemini-cli@0.64.0` | Published metadata has no scripts; propose ignore-scripts, with native dependency behavior still to validate. | [Gemini documentation](https://geminicli.com/docs/) |
| OpenCode | `opencode-ai@1.19.3` | `postinstall: node ./postinstall.mjs`; requires lifecycle review. | [OpenCode documentation](https://opencode.ai/docs/) |
| Cursor | No npm pin | Official instructions execute a mutable shell/PowerShell download. Unsupported until a fixed URL/hash download-and-verify strategy is vetted. Binary is now documented as `agent`; keep legacy `cursor-agent` as an exact recognition alias, not an automatic profile rewrite. | [Cursor installation](https://cursor.com/docs/cli/installation) |
| Grok | No vetted pin | Unsupported; do not substitute similarly named third-party npm packages. | Existing launcher ACP profile is not evidence of an official installer. |

Exact top-level package pins do not pin all transitive dependency resolutions.
npm checks registry integrity, but this is not a reviewed transitive lockfile or
offline installation. The confirmation must disclose network access, global
npm-prefix writes and lifecycle execution where enabled. A non-writable prefix
fails visibly; never retry with elevation or change PATH/system configuration.
Installation can replace an existing global package; it never restarts Agents.

## Launcher provider-missing handoff

Core's outer-executable `command_not_found` does not cover an already-running
`treer-agent` whose harness provider is absent. Proposed optional launcher
state field: `providerStatus: {status: "missing", provider_id?: <known id>,
command?: <validated bare name>, code: "command_not_found"}`. Do not infer it
from arbitrary stderr. Use the launcher's provider resolver's classified
not-found outcome; absolute/custom paths omit `command` and `provider_id`.
Keep the AIS/UI alive in a not-ready state so it can expose the fixed reason.
Do not classify authentication, model, startup timeout or malformed npm-shim
errors as missing; do not automatically install, log in, retry or change profiles.
Core/Web must confirm the DTO consumer and readiness behavior before this
startup-lifecycle change is implemented.

Next: Core/security confirm table shape and Windows expansion; inspect required
postinstall sources without executing them; commit table/validator/hash fixtures
and payload inclusion; review; add the independently reviewed missing-state
lifecycle change and sealed fake-provider tests. Full gates use
`just agent-launcher-test` through the machine-wide gate lock.
