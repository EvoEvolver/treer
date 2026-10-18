# Telegram slice: disposable release acceptance (#159)

This is a release-owned procedure, not authorization for an author to deploy,
restart or use an existing bot. Run only after exact-SHA review in the isolated
`canary-e2e` workspace. HTTP and email are outside this slice. Local fake services
do not establish live Telegram delivery, Policy isolation or native memory use.

## Provision without secrets in artifacts

1. Leader supplies one disposable bot/chat, the allowed human Telegram numeric
   ID, one ordinary endpoint Agent and one test recipient Agent. Record stable
   IDs and the test workspace/machine on the ticket, never bot tokens. Confirm
   no other poller uses this bot. Do not touch the user's existing bridge.
2. Release builds the frozen SHA, records both executable SHA-256 values and
   installs `treer-message-hub`, `treer-endpoint` and a pinned Treer CLI in an
   immutable versioned directory. Use a fresh private state directory for each
   program. See the README's relay/hub configuration field shapes; endpoint ID,
   generation, recipient and hub runtime ID must agree. No mutable author
   worktree, extra launcher script, borrowed workload identity or alternate Bot
   API origin is part of this deployment.
3. Provision the hub config and bot JSON with a private editor/secret tool:
   owned 0700 directories, owned 0600 single-link files, no symlink ancestors,
   outside VCS. The hub alone reads the bot token. Relay config contains no bot
   token. Do not pass secrets in argv, environment snippets, tickets, screenshots
   or command history. Same-UID 0600 is not isolation: record the dedicated OS
   user/container boundary and actual Policy checks separately. Same-UID processes
   can read the token file, SQLite state and held content, and act as the endpoint.
4. Create one workspace-only Managed hub App, with its own runtime identity,
   dedicated App URL and selected loopback port. Never use `--public`. Inspect
   its actual ingress access and record IDs, SHA and address. Initially keep
   `activation_enabled: false`; no browser request can enable it. Confirm the
   owner settings page is accessible and shows the operator gate disabled.

## Bind and authorize the disposable run

5. The owner reviews/applies narrow test Policy permissions for the actual hub,
   endpoint and recipient. Prove unrelated callers cannot impersonate an
   endpoint or read the private files. A monitor/default-allow workspace is
   routing evidence only; explicitly record isolation as untested in that case.
   The App never changes Policy, creates Agents or borrows owner credentials.
6. Only after release authorization, configure the disposable account and set
   `activation_enabled: true` in its private hub config. Release starts/restarts
   only these newly created test processes. This does not resume any paused
   binding. Record the exact gate value and non-secret account/bot IDs.
7. On the dedicated App URL, sign in as the current workspace owner. Create a
   paused binding using endpoint/recipient IDs and generation 1; inspect the
   Core-derived path, machine and cwd hash. Bind exactly the disposable account,
   chat/topic and allowed human IDs. Do not edit SQLite to bypass the UI.
8. Preview resume. Check current revision, relay release/profile pins, fresh
   Core metadata, verified bot status, Policy/OS isolation and held/parked counts.
   Explicitly choose either reviewed isolation or unverified isolation with risk
   accepted; neither is preselected. Same-UID functional smoke must use the latter
   and reference `functional smoke, sandbox folder, same user, isolation untested`.
   Owner declarations are evidence, not attestation by the App. Verify the
   persistent isolation-unverified badge and audit choice/reference. Confirm default
   held discard and resume only after reviewing those exact objects. A message
   before this step must not reach the recipient. Never reuse a stale preview.

## Evidence and negative controls

9. Send synthetic text from the allowed human in the bound chat. Observe the
   recipient Core Message: authenticated sender is the endpoint Agent, source
   is Telegram with the configured account/conversation, and the text is treated
   as untrusted external input. Record only message/delivery IDs and UTC timings.
   Have the recipient reply to that endpoint with the original context ID;
   observe the reply only in the configured Telegram chat/topic. Record Telegram
   message ID and UTC timing, not message text or private screenshots.
10. Repeat with an unauthorized sender and another chat; neither may reach the
    recipient. Exercise an old update/date gate using synthetic test content;
    inspect fixed counters/conditions rather than raw update dumps. Pause through
    the owner page and verify subsequent content remains held/backpressured and
    no new downstream send starts. An already in-flight send cannot be recalled.
11. Capture metadata-only status: revision/generation, bot readiness, inbound and
    outbound counters, Core acknowledgements, fixed errors and pending counts.
    Do not equate an upstream observed request with a durable local commit, nor
    a local accepted-send counter with human receipt. Telegram `sendMessage`
    lacks an idempotency key: ambiguous accepted sends can duplicate after retry.

## Drain, rollback and cleanup

12. Keep the user's old bridge unchanged throughout this test. To drain a test
    binding, stop generating test traffic, let accepted Core/Telegram work and
    acknowledgements settle, then pause it. Document remaining held, parked or
    ambiguous work; a pause is not a drain and cannot retract an in-flight send.
13. On rollback, release stops only this test hub/relay and preserves their
    private SQLite directories (including WAL/SHM, or a consistent SQLite backup)
    and private configs with owner-only permissions. Queue state is not erased
    to manufacture an empty result. Never run two hubs against one bot/state.
    Starting with the operator gate false forces bindings paused and makes no
    Telegram transport calls. Re-enabling later still requires owner review.
14. If compromise is suspected, leader revokes the disposable token at Telegram;
    changing/deleting the local file does not revoke the in-memory token. Release
    replaces the private file only after authorization. After evidence capture,
    leader/release remove only their disposable App/Agents, test rules and bot,
    and record cleanup. Real-account migration requires a separate approved
    drain/cutover/rollback plan; this smoke grants no such permission.
