# Owner connection card

The settings page puts a bilingual “Copy instructions for my Agent” card before
credential management. It reads `owner/integration` relative to the current App
root after a successful `owner/state` read. It never derives a data address from
the owner URL or Console mount.

The integration JSON contract is `schema_version: 1`, matching
`identity.{workspace,app,agent}`, `data_base_url` (root HTTPS origin or explicitly
loopback HTTP), `reachability` (`loopback_only` or
`configured_https_unverified`), boolean `enabled`, `activation_available` and
`prompt_available`, bounded `blockers`, token-free `skill_markdown`, and
`manual_path: "SKILL.md"`. Reads require no-store, stop after 10 seconds and
128 KiB, and reject foreign identities, invalid URL origins and malformed data.
The current contract has no verified-ready state. HTTPS alone never proves
external reachability. A loopback endpoint, disabled execution, missing Policy
acceptance evidence and absent unexpired token metadata are visible requirements;
unavailable prompt delivery is a separate limitation.

Copy combines a concise readiness snapshot and requirement list with the
self-contained server skill, without repeating its credential instructions.
The card supports secret environment input or a private file; it does not
require Python for every client. It never reads the one-time token textarea. Clipboard
refusal selects the visible read-only text for manual copying. The App-relative
`SKILL.md` download contains the public server-generated skill, never a token.
State refresh and owner failure clear old instructions; page exit invalidates
pending reads. Neither copy nor download activates the App or creates credentials.

Focused browser controls:

```sh
EXTERNAL_PLAYWRIGHT_MODULE=/absolute/path/to/playwright/index.mjs \
node apps/external-agent/tests/owner-integration-browser.mjs
```

The local HTTP fixture checks desktop light/English and 390px dark/Chinese,
loopback and unverified states, token exclusion, exact data URL, keyboard copy,
clipboard fallback, nested mount download, language switching, denied/unavailable,
foreign identity, oversized/unsafe responses and stale response cancellation.
It starts its own random loopback HTTP listener and closes its browser/listener.
This verifies UI behavior, not real owner authorization, data ingress mapping,
Policy acceptance or an authenticated deployed browser.
