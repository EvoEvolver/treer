# Message authentication and retry (#158)

`INGRESS-AUTH-RETRY` is a separate source successor to receipt/pressure
`87ae84cc` and accepted batching `74806250`. It changes no frozen artifact or
running target. Node's `message-watch.ts` is the reference behavior.

The watcher retries outstanding durable ACKs before receiving another poll.
Receive, batch admission and ACK must all succeed before its failure counter
resets. Authentication errors stop the watcher only; they do not stop the
provider or discard pending work. Correct the inherited credentials and restart
through the existing supported lifecycle to resume; this change performs no
automatic credential/config reads or restart. Durable pending ACKs survive and
are retried with their original operation keys after restart.

Other failures use Node's one-second base, exponential factor with bounded
0–20% jitter, and 60-second cap. An empty successful poll waits 250 ms; successful
nonempty polls can immediately receive again. This replaces Rust's fixed
two-second retry. The counter is bounded even under indefinite outage.

Only fixed `message_auth`, `message_network`, `message_other` categories and
numeric failure/delay fields are logged. Failed CLI responses are classified
from bounded stdout/stderr prefixes: known structured error codes, line-leading
authentication codes or standard HTTP 401/403 status diagnostics indicate auth;
known connection failures indicate network. Unknown failures retry as other.
Bare mentions in message prose are not credential errors. Stderr is drained
concurrently to prevent pipe deadlock while retaining at most 16 KiB; failed
stdout contributes at most 16 KiB to classification. Raw diagnostics never
enter logs or durable state. Stdout still has the existing 16 MiB response cap;
the existing 40-second command deadline and owned-child cleanup remain bounded.
This is a narrow classifier for supported command errors, not an exact copy of
Node's regex language or a claim of universal protocol parity.

Targeted tests cover category privacy and jitter bounds, actual receive auth
stopping without repeated attempts, failed auth ACK retaining both pending
identities across restart without turn replay, large stderr pipe drainage, and
network backoff growth/reset after a successful admitted/ACKed poll. Existing
batching and receipt entry tests remain applicable. No live credential failure,
provider-native files, migration, lifecycle, browser or whole gate is exercised.

Operationally, a future leader-selected artifact must pin this successor and
preserve its existing startup/thread/receipt/ACK state. It is not silently added
to the accepted batching-only UI runtime. Rollout expansion stays paused; source
completion does not authorize deployment.
