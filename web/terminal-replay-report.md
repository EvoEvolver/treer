# Terminal replay query feedback

## Local reproduction

At baseline `ac914fe`, a mocked terminal WebSocket supplied two historical output
chunks containing OSC 10/11 color queries, DA1/DA2, CPR and DSR, followed immediately
by live queries. The browser sent 15 binary input frames: eight historical replies
and seven expected live replies. The failure occurs after frame receipt has already
decremented the replay counter, because xterm parses writes asynchronously. This
test uses synthetic output, an isolated test home and mocked API/WebSockets; it
does not attach to any real Agent or read its output.

## Fix and verification

Chosen solution: B, parser handlers. Install handlers when `ready` declares
replay chunks; swallow CSI c / > c, CSI 5 n / 6 n / ? 6 n and OSC 4/10/11/12
commands containing a query (`?`). Remove them in the final replay write callback,
after parsing and before the following queued live write. Terminal disposal also
removes handlers. Ordinary color setters continue through the default parser;
mixed color setter/query commands are swallowed as a whole during history replay.
This is deliberately scoped to these query families, not a universal guard for
every possible terminal extension. No input-pattern filter, timeout, private xterm
API or serialization of output writes is used.

Browser regressions cover split queries across replay chunks, queued live queries,
foreground/background replies, primary/secondary device attributes, normal/private
cursor positions, status/cursor/palette queries, user typing during unfinished
replay, reconnect with/without reset, and page reload. Both desktop and 390px use
the same component. A Managed App embedding regression switches from its human
iframe to logs, verifies silent historical queries and unmodified user input.
Existing output-burst/backpressure regressions cover batching and acknowledgements.

## Client scope

`WorkspaceApp` renders `TerminalPane` for desktop terminals, browser mobile
full-screen terminals and Managed App logs. `embed-ui.spec.ts` exercises the same
component when a workload uses the terminal/logs surface. An Agent Interface's
own iframe is supplied by that Agent and does not use this component.

Native iOS `AgentTerminalScreen.swift` and Android `AgentTerminalScreen.kt` use
their own text-based emergency terminal views; they do not instantiate xterm or
automatically answer these queries. Their Agent WebViews load separate interfaces,
not the console `TerminalPane`. No native source change is needed for this fix;
independently supplied iframe/WebView terminal implementations are outside its scope.

## Impact and recovery limits

Any running Agent whose PTY history includes these queries and whose terminal or
logs was opened/reconnected/refreshed in the affected browser console may have
received additional input. This can include Codex, Claude Code, other TUIs and
shell sessions; it is not limited to the Agent currently selected by the leader.
Each replay can add another copy. Depending on the PTY/TUI mode, replies can be
handled as terminal responses, echo as visible garbage, pollute an unsent prompt,
or be interpreted as key actions. They do not inherently include an Enter key;
there is no local evidence that a command was executed or an Agent submitted a
prompt. Concurrent browser attachments can compound the feedback.

We cannot identify specific polluted Agent IDs without inspecting their actual
sessions. No real-session inspection, input clearing, command execution or restart
was performed. The fix prevents new historical replies after the corrected console
is loaded; it cannot retract bytes already delivered or remove garbage already
saved in PTY history. A human should inspect the affected input box and discard
unwanted unsent text before continuing. Rollout and any coordinated recovery remain
with leader/release; this task does not deploy or restart services.
