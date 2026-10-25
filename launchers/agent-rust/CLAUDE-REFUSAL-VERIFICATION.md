# Claude refusal diagnostics (#247)

Base: 6d6840dac69286c72561a78bdf7849a5adfb397e.

## Actual incident and recovery

The expert interface on generation11 reported ready=false,
transport=claude_protocol_refused, recovery=integrity_failure, and a confirmed
provider exit after launcher cleanup. All35 supported queue receipts were done
with null errors; recovery.unresolved was empty. Thus this incident was a real
protocol fence, not a running receipt's exact_turn_pending label. The old reader
discarded the adapter's exact error. The triggering frame and specific failure
cannot be reconstructed from the remaining public diagnostics. This change is
diagnostic preparation, **not a claimed root-cause fix** or permission to relax
unknown-output/terminal/receipt authority.

After notifying the user, the author backed up normal startup and a canonical
digest of the ordered queue projection and restarted only expert once. Generation
12/PID22547 became ready/idle/transport open, same session/cwd/startup. The old35
records' prefix digest matched exactly. One newly delivered normal message
notification created a distinct36th receipt, which completed; it was not a replay
of an old submission. No input was submitted by the diagnostic driver and no
receipt was cleared/disposed. No other Agent/Host/Controller/default was changed.
The restart used the existing6d production package, not this diagnostic source.
Safe before/after records and startup backup are in the task worktree's ignored
`.treer/reviews/expert-recovery/` directory.

## Changed boundary and verification

The reader publishes only an explicit allowlist of fixed adapter error codes,
two state booleans and a bounded run count. Arbitrary errors fall back to the
generic code. Neither native frames nor identifiers, message content, arbitrary
error context or credentials are retained. The existing fatal refusal, cleanup,
unknown/no-replay behavior is unchanged.

- Four focused adapter units PASS, including arbitrary/private error text refusal
  and the existing interrupt/task/no-false-settlement controls.
- Two actual fixture native entries PASS: wrong task origin and unsolicited stream
  now expose distinct refusal codes while preserving completed/uncertain receipts
  and refusing replay. The old source lacks protocolFailure and fails these new
  assertions. These are synthetic producer cases, not the unknown incident frame.
- Feature-harness build and strict all-target Clippy PASS; docs and diff checks PASS.
- No provider account test, browser matrix, full gate or shipping build was run.
  The diagnostic binary used for checks is fixture-enabled and must not ship.
  Release must produce a new ordinary native image before this diagnostic can be
  used on a managed expert session; existing6d remains unchanged meanwhile.
