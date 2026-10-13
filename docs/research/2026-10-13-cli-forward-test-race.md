# CLI forwarding integration-test port race

The release gate for `78dad4a` failed at `session.rs:238`: reading the reply
after a TCP write-half-close returned macOS `ConnectionReset` (54). Authentication
assertions did not fail. The fixture uses only local fake Controller sockets;
neither PostgreSQL nor NATS participates.

Independent worktrees, each with its own build target, compared `78dad4a` and
released `6b0b561`. The forwarding implementation, test and dependency lockfile
are identical between those commits. Each run below starts a fresh test process;
failures count as failures, rather than being replaced by retries.

| Unmodified revision | Single forwarding test | Full five-test session suite |
| --- | --- | --- |
| `78dad4a` | 50 passed, 0 failed | 47 passed, 3 failed |
| `6b0b561` | 50 passed, 0 failed | 49 passed, 1 failed |

Temporarily collecting CLI stderr on a failed read reproduced the failure at
iteration 75 of an additional candidate-suite diagnostic run. The response was
empty and stderr reported `treer: Address already in use (os error 48)`.
This establishes a failed bind, rather than a forwarding half-close failure.
The precise competing socket was not identified.

The fixture bound an ephemeral port, released it, then asked the CLI to bind
that port. Other parallel sockets can reuse the released port. A successful
test connection does not establish that this CLI owns the listener; the CLI
can fail its bind while the test connects to a competing socket and gets reset.
This race predates the candidate's security and creation-placement changes.

The fix requests `127.0.0.1:0` from the actual CLI and reads its existing
`treer: forwarding on <address>` readiness line, emitted after successful bind.
Only then does the test connect to the actual allocated address. There is no
released reservation, readiness polling or retry of a failed assertion.
The binary payload, write-half-close, response-after-EOF and authenticated
workload-header assertions remain intact. No production code changes.

With this fix, 100 fresh executions of the complete five-test session suite
passed, with zero failures. This local diagnostic evidence does not replace the
complete release gate, which must run again against the fixed candidate.
