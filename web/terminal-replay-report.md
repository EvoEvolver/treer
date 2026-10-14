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
replay chunks; swallow the source-audited query inventory below. Remove them in the final replay write callback,
after parsing and before the following queued live write. Terminal disposal also
removes handlers. Ordinary color setters continue through the default parser;
mixed color setter/query commands are swallowed as a whole during history replay.
This covers built-in query responders in xterm 5.5.0, not arbitrary future terminal
extensions or non-query focus/mouse event reports. No input-pattern filter, timeout, private xterm
API or serialization of output writes is used.

Browser regressions cover split queries across replay chunks, queued live queries,
foreground/background replies, primary/secondary device attributes, normal/private
cursor positions, status/cursor/palette queries, user typing during unfinished
replay, reconnect with/without reset, and page reload. Both desktop and 390px use
the same component. A Managed App embedding regression switches from its human
iframe to logs, verifies silent historical queries and unmodified user input.
Existing output-burst/backpressure regressions cover batching and acknowledgements.

## xterm 5.5.0 query inventory

Confirmed the installed package version and audited all `triggerDataEvent` sites
in `src/common/InputHandler.ts` and `src/browser/Terminal.ts`, including the
indirect color, window-report and focus events. Parser registrations come from
`InputHandler`'s constructor. The implementation is centralized in
`src/lib/terminal-replay.ts`; browser tests instantiate the real xterm with window
reports explicitly enabled, split every query across writes and queue the live
copy before replay parsing finishes. Each row is tested for silent replay and
unchanged live behavior.

| Query | Source responder | Live behavior |
| --- | --- | --- |
| CSI c (DA1), CSI > c (DA2) | `sendDeviceAttributesPrimary/Secondary` | Device attributes |
| CSI 5 n, CSI 6 n, CSI ? 6 n | `deviceStatus/Private` | Status and cursor position |
| CSI ? 15/25/26/53 n | `deviceStatusPrivate` | **No reply in 5.5.0**: response calls are commented out; guarded defensively |
| CSI Ps $ p, CSI ? Ps $ p (DECRQM) | `requestMode` | ANSI/private mode status, including unknown modes |
| DCS $ q Pt ST (DECRQSS) | `requestStatusString` | SGR, margins, cursor style, protection, conformance; negative reply for unknown requests |
| OSC 4;index;? | `setOrReportIndexedColor` → browser `_handleColorEvent` | Palette color(s) |
| OSC 10/11/12;? | `_setOrReportSpecialColor` → browser `_handleColorEvent` | Foreground/background/cursor colors, including stacked queries |
| CSI 14/16/18 t | `windowOptions` → browser `_reportWindowsOptions` or direct reply | Window/cell pixels and character dimensions; option-gated, disabled in console |

CSI 14;2 t is unimplemented. Window/icon title reports (CSI 20/21 t) are listed
in comments but have no responding switch branch in 5.5.0. OSC clipboard queries
and DCS capability requests are not registered built-in responders. No ESC query
registration emits a response. DECSET 1004 enables focus event reporting and can
immediately report focus via `_reportFocus`; subsequent focus/blur and mouse
reports are events, not queries, and remain outside this guard. Key/composition,
paste and selection-generated input also remains unchanged. Re-audit this inventory
when upgrading xterm rather than assuming future responder coverage.

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
