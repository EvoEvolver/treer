# Disposable Managed App messaging smoke

This **test helper is Python, not a third product program or the Rust hub**.
It tests the App-kind runtime identity against actual Core before the hub exists.
No Telegram/email/HTTP adapter, HMAC, bot token or real user content is used.
Its only mutation is a synthetic Core Message sent to its own temporary runtime.

Release-only procedure, in the dedicated canary-e2e workspace/machine:

1. Export the frozen commit under a new workspace-root-relative directory
   `treer-endpoint-smoke-SHA` (replace SHA with the full reviewed commit). Do not
   reuse a mutable author worktree, existing App or existing name. Inspect the
   installed CLI absolute path and available unprivileged port first.
2. Create one **temporary private Managed App**, named
   `endpoint-runtime-smoke-NONCE`, with cwd `treer-endpoint-smoke-SHA` relative
   to that machine's workspace root. Saved command (substitute only operator
   inspected absolute executables, selected free port and a fresh 8–48-character
   alphanumeric NONCE):

   ```text
   /absolute/python3 apps/message-endpoint/tests/app-runtime-smoke.py
     --cli /absolute/treer --nonce NONCE --port PORT
   ```

   Never pass a workload credential or another Agent ID. The process gets its
   own runtime ID from Treer. The test may need an authorized human restart of
   **only this newly created disposable App** after rules are installed; no
   existing process may be touched. Prefer preinstalled workspace test rules
   when possible. App starts failed if denied, with `/health` returning503.
3. Permission fixture `runtime-smoke-policy.json` contains only this smoke's
   required allow rules. Substitute every `SMOKE_AGENT_ID` with the runtime ID
   returned by creation (JSON operation, no shell interpolation). The canary-e2e
   **owner** applies/merges these three rules through the normal Policy route;
   this helper never reads or edits Policy. Do not replace unrelated rules or
   claim this fixture is the production isolation template. Existing higher
   priority denies can still block it. If canary-tester is not owner or cannot
   apply rules, record that blocker and request leader action; no new accounts,
   borrowed owner token, changing provider or production policy edits.
4. Inspect `/health` through the temporary App's workspace-auth ingress. Exact
   passing response has `status:passed` and these seven checks:
   `self_send`, `same_key_same_id`, `authenticated_sender`, `receive_from_app`,
   `redelivery_before_ack`, `ack_replay`, `no_redelivery_after_ack`.
   A failed check returns503 with no body/credential/raw CLI error disclosure.
   Report App ID/runtime kind, frozen SHA, check names and outcome on #159.
5. Delete only this temporary App and its test Policy rules and exported
   directory after result capture. Confirm no smoke processes remain. Keep no
   public App, real bot, transport secret or retained user messages.

This does **not** assert multi-hop routing, fragment assembly, relay/hub code,
Policy template isolation or memory targets. Those get their own P2 code tests
and later executable freeze. Fragment limits (1/2/16 parts and256KiB) are currently
hermetic Rust tests; do not label this self-message smoke a fragment acceptance.
