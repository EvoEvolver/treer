# External Agent App: isolated acceptance handoff

## Incremental output and disable client

`tests/output_containment.py` adds a separate nine-request leaf to the retained
private 27-check and Policy-change 30-check evidence. Completed roots/tokens are
not replayed. Release provisions a fresh isolated stack and token, with a stable
synthetic allowed target and another target explicitly denied `agent.output.read`.
The App executable is unchanged; no product build or whole gate is needed.

Use the exact `tests/acceptance.py` manifest plus `expected_output_sha256`, the
lowercase SHA256 of the full UTF-8 synthetic output text (not JSON serialization).
Pin the current target's complete bounded, non-truncated output before execution.
Do not use private real Agent output or record output bodies in the safe receipt.

Run `python3 apps/external-agent/tests/output_containment.py /private/new.json`
to validate without credential reads/network. Add `--execute MANIFEST_SHA256`
to execute within the separately bound fresh fixture scope.

Execution makes exactly nine requests: owner state, audit baseline, allowed
output, denied output, correlated audit, owner disable, disabled state, refused
output, final unchanged audit. Allowed output must match Agent identity, revision
shape, non-truncated flag and digest. The denied output must return typed Policy
denial. Both dispatches must match token/operation/target/outcome audit rows.
Disable binds the original generation and execution Agent; the next request must
return `503 disabled` without another execution row. No enable, token creation,
revoke, prompt, restart, Policy mutation or automatic retry occurs. A failed
disable response is uncertain: inspect through independent control, never resend
blindly. Failure before disable may leave the fresh fixture enabled; the execution
owner retains containment and cleanup duties on every outcome.

Release separately proves independent exact-runtime stop and owned process/port
cleanup. Public ingress, IP attribution, browser rendering and elapsed expiry are
not proved here. Client mocks prove mechanics only; actual new acceptance remains
NOT_RUN until the execution owner records a fresh-stack receipt.

Status: **offline preparation only; every live check below is NOT RUN**.
Release owns fixture provisioning and release coordination. The previous #224
blanket hold is lifted. Core still reviews the narrow Controller contract.
The fixture may use an existing approved machine with exact task-owned temporary
processes/Agents/Apps; the entire machine need not be disposable. Record original
resource ACKs and bound cleanup/independent stop to those resources only.
This document neither opens a listener nor authorizes activation, a restart,
credential creation, or public traffic. Decide the exact live boundary after
Release has prepared the fixture inventory. Keep #203/#204 production untouched.

The local executable/browser smoke uses fake upstreams. Its passing result is
development evidence only. Do not copy its assertions or evidence hashes into
live configuration. See [README.md](README.md) for runtime limits and recovery.

## Fixture inventory to prepare

Record actual IDs privately before running anything; do not infer identities
from names. Leave unresolved fields empty rather than inventing usable values.

| Inventory | Required property |
| --- | --- |
| Candidate | Exact App SHA and hash, reviewed Core route contract, #224 release SHA and gate/CI evidence |
| Workspace A | Explicitly approved fixture workspace/scope on an existing machine; selected real enforcing provider, task-owned synthetic targets; no unrelated grant or resource changes |
| Execution App/Agent | Fixed A workspace/machine/Agent/App/service binding, dedicated workload credential and private new state; activation off |
| Allowed target | Disposable Agent in A with synthetic output/conversation, predictable fake harness and no provider credentials |
| Denied target | Different disposable Agent in A with a unique synthetic transcript sentinel and observable lifecycle |
| Workspace B | Separate disposable workspace and target; no membership for execution identity A |
| Owner/member | Fixture human owner and member, fresh Proxy-verified assertions from real authenticated Console; no hand-minted claims |
| Origins | Actual dedicated public URL, Console URL, authenticated embedded tunnel URL, service audience, observed forwarded Host |
| Independent control | Authorized operator can stop the execution App Agent through Console/Controller without calling this App listener |
| Ingress | Two verified port mappings: anonymous data and workspace-only management with its exact service audience; existing Proxy ServiceIngressLimits/effective values and trusted-front derivation; independent App parser pools and global/token bounds |
| Private evidence | Dedicated directory outside checkout, mode 0700; credentials/config/state 0600; logs body-free and redacted |

Retain the pre-test Policy bundle revision/hash, provider selection, enforce and
failure/staleness settings. Do not reuse an empty monitor bundle or a fake
provider. Prefer the reviewed existing Policy editor/validation flow; do not
overwrite a live workspace bundle using a fixture script.

## Policy preparation

Use existing Policy, not an App-side permissions engine. Scope rules to the
actual execution Agent ID and actual resource IDs. Arrange explicit denials for
unrelated output/transcript and stop, with explicit allows for the disposable
allowed target. Use deny defaults for the exercised actions so an accidentally
missing rule cannot turn a negative into an allow. Discovery filtering and
read/start/restart/prompt action mapping must match Core's reviewed contract;
do not guess an action name from the HTTP path. The exact reviewed mapping is
in [README.md](README.md): transcript/output use `agent.output.read`; stop uses
`agent.stop`; start uses machine `agent.create`; restart requires both.
Controller health alone is not authenticated readiness; retain the own-Agent
identity/Policy check as well. Human owner control/recovery
must remain available independently of the execution Agent's grant.

Before enabling external execution, exercise the fixed workload identity
directly through the real Controller and retain the denied unrelated transcript,
denied unrelated stop, and cross-workspace refusal. This provides real negative
evidence without activating the public App. No human/operator credential may
substitute for the App workload credential. Record timestamps, exact Core/App
versions, provider revision/hash, target IDs, HTTP statuses and permitted error
codes. Prove the denied sentinel and lifecycle mutation never reach the caller.
Do not store transcript bodies or credentials in the evidence report.

The configured acceptance hash is an owner acknowledgement of retained evidence,
not runtime proof or permission to activate. The subsequent live decision must
name the exact evidence/candidate, fixture, traffic limits and rollback path.

## Ordered live matrix (pending the live decision)

| Step | Operation and expected evidence |
| --- | --- |
| 1 | Verify #224 release prerequisite and exact artifacts; real Controller preflight returns the configured workspace/machine/Agent and prompt capability |
| 2 | Verify independent stop authority before listener exposure; verify existing Proxy per-service/per-IP limits and header sanitization, anonymous owner refusal and private revocation under public parser saturation; never treat the App tunnel peer as a distinct client |
| 3 | Retain the three real Controller Policy negatives described above; no public activation yet |
| 4 | Real owner Console creates a named 30/90-day token and sees plaintext once; member, missing/wrong audience, expired assertion and foreign origin cannot manage tokens |
| 5 | Owner checks exact generation/Agent/evidence and enables only within the decided boundary; record the body-free owner audit event |
| 6 | Real non-browser external client sends bearer in a private header/config file; allowed status/output works, denied transcript/stop and foreign-workspace target fail; no denied sentinel or changed target lifecycle |
| 7 | Publish a real deny for the previously allowed operation; wait only for the documented Core refresh/invalidation bound, record observed revision and verify the next fresh call denies; restore only the fixture's reviewed bundle |
| 8 | Prompt to disposable harness carries the untrusted provenance marker and returns the queued receipt; no terminal/CLI fallback. Stop/start/restart only the disposable target within the decided traffic budget |
| 9 | Create a second token; correlate distinct token IDs with App/Agent audit identity, verify both execute as the App and neither as the human owner |
| 10 | Revoke one token, then issue a fresh request: denied and no new downstream call. The other token still follows Policy. Already dispatched calls may finish; never call that a revoke failure or replay an uncertain mutation |
| 11 | Expiry uses a disposable token/state with controlled time or a genuine expired fixture; do not change shared-machine time or live DB timestamps. If unavailable, mark NOT RUN, not PASS |
| 12 | Restart only if separately included in the live decision: revoked/expired token stays denied, outstanding records become unknown without automatic resend |
| 13 | Run the real deployed browser matrix below; retain actual response headers and per-step results |
| 14 | Verify independent Console/Controller stop with the App listener unavailable; new external calls cannot dispatch. Preserve private audit state, reconcile unknowns, leave App stopped/disabled unless the decision explicitly permits continuation |

For every negative, capture downstream evidence of non-execution, not just a
client error. Allowed calls must exercise the real Controller/Proxy/provider;
test doubles, intercepted Playwright responses and localhost gateways cannot
fill live result cells. After a failure, stop at the failing boundary; never
loosen Policy, browser-origin rules or assertion checks to make a test pass.

## Deployed browser matrix

Use actual deployed headers and authenticated fixture owner access. For both
dedicated and authenticated embedded origins record Chromium, Firefox and WebKit
results where embedding applies. Verify HTML/Markdown negotiation, relative
assets, CSP/frame ancestors, token creation/name/expiry, one-time secret display,
pagehide clearing, list/last-use, revoke, and safe rendering of a hostile name.
Verify keyboard access/focus, both languages, owner/member distinction, foreign
origin mutations, forged headers/assertions, logout ending authority and no
browser CORS access with an external token. Cookie, bootstrap, popup and embed
compatibility checks belong to the deployed Console contract: use #207's fixture
flow and mark any inapplicable step with its reason, never assume local smoke
proves deployed behavior.

Do not retain browser traces, HAR files, console/request dumps or screenshots
while secrets/assertions are present. Store only sanitized status/header names
and approved values; never Set-Cookie, bearer, workload credential or assertion.
Do not put tokens into command arguments, shell history, URLs or prompts.

## Evidence report template

Copy to the private acceptance directory; initial values stay NOT RUN:

```text
App SHA / artifact hash:
Core and #224 prerequisite SHA / release decision reference:
Route-contract review reference:
Fixture workspace A/B, execution and target IDs (private inventory reference):
Policy revision/hash, enforce and failure/staleness settings:
Ingress split-port enforcement evidence / independent stop evidence:
Live decision reference, exact permitted operations and request budget:
Real Controller negatives: NOT RUN
Public client allow/deny and Policy change: NOT RUN
Prompt receipt / target lifecycle: NOT RUN
Owner/member / revoke / expiry / optional restart: NOT RUN
Audit identity and downstream non-execution checks: NOT RUN
Dedicated / embedded browsers and deployed headers: NOT RUN
Secret-leak review: NOT RUN
Final App disabled/stopped state and unknown-outcome reconciliation:
Evidence file hashes, UTC start/end and operator:
Failures, skipped steps and residual risks:
```

Release publishes only a sanitized result summary with exact SHAs and evidence
hashes. Any NOT RUN mandatory row blocks claiming acceptance or closing #219.
Release still owns exact-candidate gates/CI, deployment, rollback and cleanup;
retire only the recorded task-owned resources and preserve uncertain evidence.

## Bounded client (prepared, live NOT RUN)

`tests/acceptance.py` is a small client for Release's **already provisioned**
fixture, not an installer. It never creates identities/tokens, changes Policy,
activates/restarts an App or acquires credentials. The default command only
validates the manifest and prints its SHA256; it reads no credential files and
opens no connection:

```sh
python3 apps/external-agent/tests/acceptance.py /private/fixture.json
```

The exact manifest fields are:

```json
{
  "public_url": "https://data.example/",
  "owner_url": "https://settings.example/",
  "workspace": "ws_disposable_a",
  "foreign_workspace": "ws_disposable_b",
  "app": "app_fixture",
  "agent": "ag_execution",
  "allowed": "ag_allowed",
  "denied": "ag_denied",
  "foreign": "ag_foreign",
  "generation": "exact_current_generation",
  "decision": "exact Release execution scope reference",
  "policy_evidence_sha256": "REPLACE_WITH_REAL_EVIDENCE_SHA256",
  "token_file": "/private/existing-disposable-token",
  "owner_headers_file": "/private/existing-fresh-human-ingress-headers.json"
}
```

HTTPS origins are normal. Literal loopback HTTP origins (`127.0.0.1` or `::1`)
are also accepted for Release’s real local stack; hostnames and non-loopback
cleartext origins are refused. This changes transport compatibility only, not
the nine-request scope or any real-evidence requirement.

`owner_url` may instead be the fixed authenticated Proxy embedding base URL,
for example `http://127.0.0.1:18081/api/workspaces/ws_disposable_a/apps/app_fixture/ui/`.
Its workspace and App must exactly match this manifest. Only this canonical
prefix (with optional trailing slash) is accepted; arbitrary paths, escaped
segments, query strings and response-selected URLs are refused. The client
appends each fixed `/owner/*` path to that prefix and sends the Proxy origin
alone in `Origin`. An existing owner cookie goes only to that owner origin;
the Proxy issues/verifies its normal service-audience assertion. The App still
has no cookie fallback. Public requests remain on their separate root origin.
The prefix is included in the manifest hash and does not add requests or change
the nine-request sequence. This HTTP client does not prove browser behavior.

The credential files must be owned regular files, 0600, not symlinks. The token
file contains a previously issued **disposable** token. Owner headers contain
only the existing human ingress `Cookie` or fresh `X-Treer-Identity-Token`;
actual Proxy verification still applies (including stripping forged input
headers). No cookie fallback exists inside this App. No secrets go in argv or
reports. Use actual service origins and pre-established human access; this tool
does not implement login, OAuth or assertion minting.

After the exact nine-request scope is decided, `--execute MANIFEST_SHA256`
checks the scope hash, then performs: fresh owner state and audit, allowed
status, denied transcript, denied stop, foreign transcript refusal, exact audit
correlation, owner revoke, and fresh revoked denial. No retries, redirects,
environment proxy, parallel traffic or extra requests. A generic unavailable
response never counts as a Policy denial. Expected foreign response is masked
503/unavailable; Core must separately prove the target's foreign-workspace
binding and non-execution. Every new audit row must have this disposable token,
operation, target, and expected outcome. Stop at the first mismatch; a failure
before the revoke step leaves cleanup to the independent operator.

A manifest hash/evidence pin is a scope binding, not proof that enforce mode or
provider provenance is real. Release retains those real upstream facts and
denied-target non-execution evidence separately. The client deliberately reports
those as EXTERNAL EVIDENCE REQUIRED; browser, expiry, Policy-change and
independent-stop results remain NOT RUN. `test_acceptance.py` validates client
mechanics only and cannot populate any real acceptance result.
