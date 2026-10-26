# Prepare a human email endpoint (#204)

The deployed Hub already uses Treer Proxy email mode. No SMTP account or provider
key is required. `activation_enabled:false` keeps external delivery paused while
setup is completed; `telegram_accounts:[]` does not prevent email setup.

Choose an owned **command Agent** for the relay and a real internal Agent as the
conversation counterpart. Do not adopt an old sandbox relay by name. If none is
owned, a managed installer can create a fresh stopped identity on the intended
machine using the existing CLI (the one-shot command exits without mail):

```sh
treer agent admin create --machine MACHINE_ID --kind command \
  --name human-email-relay --cwd MACHINE_RELATIVE_WORKDIR -- /usr/bin/true
```

Read its actual returned stable Agent ID/path and wait for its stopped/exited
state. Use a private directory outside any checkout, with the matching native
`treer-endpoint` and trusted `treer` binary present **on that relay machine**.
Run the helper there from an existing managed installer identity in the same
workspace. Obtain binary hashes from the verified release receipt, not a remote
request or guessed path.

```sh
python3 /ABS/SOURCE/apps/message-endpoint/scripts/prepare-email-relay.py \
  --workspace WORKSPACE_ID --hub HUB_APP_ID \
  --endpoint OWNED_RELAY_AGENT_ID --recipient COUNTERPART_AGENT_ID \
  --cli /ABS/treer --cli-sha256 VERIFIED_CLI_SHA256 \
  --relay /ABS/treer-endpoint --relay-sha256 VERIFIED_RELAY_SHA256 \
  --output /ABS/PRIVATE_0700_PARENT/new-email-relay --generation 1
```

This performs three bounded `agent show` reads, verifies workspace/kind/placement
and stopped relay state, and writes an exclusive private bundle. It neither
creates a channel nor changes Hub configuration, Agent startup or activation.
`relay.json` pins the real IDs and binaries; `launch.json` contains exact argv for
saving startup and starting through the Controller. It contains no credentials.
The caller must run on the relay's own machine; inherited credentials are used
only in memory for normal CLI authentication.

An authorized managed installer can execute the two separately visible argv
steps below. Review `launch.json` first; **do not replay a start after an uncertain
response**. Startup replacement preserves the stopped intent; Controller start
supplies the relay's own workload identity. Never inject the Hub's identity or
copy a workload credential into this bundle.

```sh
python3 - /ABS/PRIVATE_0700_PARENT/new-email-relay/launch.json startup_argv <<'PY'
import json, subprocess, sys
subprocess.run(json.load(open(sys.argv[1]))[sys.argv[2]], check=True)
PY
# After confirming saved startup, run the same command with start_argv.
```

Open the existing private Hub owner page. In **Create endpoint**, enter that relay
ID, fixed counterpart and generation; inspect the fresh Core preview and confirm
the **paused binding**. The helper's `preview-create.json` is also the exact
existing configuration API preview request; never manufacture the returned
preview. In **Email setup**, select the current human member and enter their
matching recipient email address. Bind the contact at the current revision.
Owner authentication and existing CSRF checks apply normally.

Before enabling, inspect the relay startup status and binding pins, then use the
existing preview/resume controls with the global runtime activation gate enabled
by the deployment owner. Real external sending is a separate explicit step with
the intended recipient and content; setup never sends a test email automatically.
The matching Core revision under #204/9252 checks App identity, `email.send`
Policy, address syntax, rate and idempotency; it does not require recipient
membership or `TREER_EMAIL_ALLOWLIST`. Hub still checks its own member/contact
binding for this conversation and browser-reply workflow.
Outbox means queued locally; platform acceptance is not inbox delivery; an
unknown send outcome is not automatically retried.

The current remaining human inputs are the recipient's member/address association
and the chosen relay's ownership (or a decision to use a fresh owned relay) plus
its fixed internal counterpart. An unavailable browser session is not a reason
to rerun source gates. The installer stages executable material; the actual human
can finish contact binding in the existing owner UI.

## Focused setup verification

```sh
TREER_SETUP_TEST_RELAY=/ABS/BUILT/treer-endpoint \
  python3 apps/message-endpoint/tests/test_prepare_email_relay.py -v
```

This covers private bundle/no credential copying, wrong pin/workspace and running
relay refusal, bounded discovery, and generated-config native relay startup,
forward and ACK against a closed synthetic CLI. It is not real Core or email
provider delivery evidence. It starts/stops only its own native test process.

## Minimize human input without guessing identity

Start with the normal read-only `treer member list` and `treer agent show -- ID`.
The member directory supplies current names, IDs and roles, **not email
addresses**. An owner role or matching display name does not prove mailbox
ownership. Existing Agent settings deliberately mask contact mappings; do not
extract private SQLite state, credentials or another browser's session to fill
that gap. The existing human-owner **Email setup** preview supplies fresh member
choices; email contact binding remains the explicit member/address assertion.

Prepare a single candidate sheet with evidence beside each field:

| Field | Agent can prepare | Remaining fact or decision |
| --- | --- | --- |
| Member | Current directory ID/name/role | Confirm member-to-mailbox association if normal API does not expose it |
| Mailbox | An explicit recent recipient instruction | Historical test addresses alone are not current binding evidence |
| Relay | Fresh exact ID/kind/placement/stopped readback | Missing record is a technical reconciliation task, not permission to create a replacement |
| Counterpart | Existing task-owned integration Agent as a proposed default | State that it is a proposal, not an already selected contact |
| Content | Harmless source summary; existing notification template | Actual runtime thread ID/link must exist before sending |
| Count | Exact one-message candidate, stable existing runtime idempotency | No resend after unknown outcome; inspect the journal |

The normal runtime subject is `Treer conversation update`. Its external body is
`New Agent message in thread <actual thread ID>` plus the authenticated thread
link, not the underlying Agent message. Prepare the source summary separately,
for example `#204 email endpoint connectivity check. No sensitive content.`
Do not fabricate a thread hash just to make a candidate look executable.

After technical facts are available, one confirmation can cover the proposed
member/mailbox association, counterpart, exact rendered notification and count.
A yes/no candidate with unknown relay or thread fields is **not ready to send**.
Offer a separate read-only alternative: “Continue current directory/metadata and
owner-authorized existing setup preview checks only; no binding mutation,
activation, installation, replacement relay, token handling or email.” This can
advance fact gathering without authorizing delivery. Only the runtime owner
executes any later separately authorized change; no additional setup system or
provider credentials are needed.

## Independent managed bootstrap when historical relay records are unavailable

The **current implementation needs a relay**, not either historical relay ID.
The email worker consumes outbound envelopes stamped by that relay, and sends
member replies back through its pinned Core mailbox. Neither `agent_not_found`
nor a create ACK proves whether an old Controller record was deleted. Preserve
both old IDs, their state and ACK evidence for separate reconciliation. That
operator investigation need not be a product prerequisite for a newly isolated
binding. A direct no-relay email mode is not implemented and must not be claimed.

The supported incremental route uses the normal Policy-controlled
`agent admin create` under an authorized managed installer identity. A **new**
private directory, immutable Tree name and generation isolate its startup and
Hub binding from old records. The new bootstrap solves the ID/config chicken-and-
egg: it obtains its own Agent ID from Treer's injected environment, writes a
pinned config exclusively, and execs the already shipped native relay. It does
not call create, mutate Hub settings or enable email. On Host recovery the same
saved bootstrap reuses only an exactly matching config; another ID cannot adopt
that state. Unlike the historical `/usr/bin/true` staging recipe, it immediately
runs the real long-lived relay; this is not a claim to fix the unexplained old
projection failure.

Before any live creation, the runtime owner gives the leader this exact impact:
**one new command Agent, one new private directory/state, one new paused Hub
binding; zero changes to either historical Agent, old mailbox/state, Hub gate,
Host or Controller; no automatic migration or old-message replay.** Creation is
one dispatched operation. If its response or independent readback is ambiguous,
record the intent and stop; do not issue another create. Its installation name
and directory must be unique. Removal or stopping is a separately scoped action,
not automatic cleanup of an uncertain result.

Prepare `bootstrap.json` as an owned mode0600 regular file in a new mode0700
physical directory outside git. All paths refer to that same machine. Example
schema (replace from real readbacks and verified release pins):

```json
{
  "workspace_id": "ws_FROM_CURRENT_CONTEXT",
  "server_id": "srv_FROM_CURRENT_CONTEXT",
  "hub_id": "app_CURRENT_HUB",
  "recipient": "ag_SELECTED_COUNTERPART",
  "generation": 1,
  "cli": "/ABS/PINNED/treer",
  "cli_sha256": "VERIFIED_64_HEX",
  "relay": "/ABS/PINNED/treer-endpoint",
  "relay_sha256": "VERIFIED_64_HEX",
  "directory": "/ABS/NEW_PRIVATE_DIRECTORY"
}
```

After leader coordination, the runtime owner can execute the normal create
contract once; these are placeholders, not a command to run without staging:

```sh
treer agent admin create --machine ACTUAL_MACHINE --kind command \
  --folder /treer-dev-team --name UNIQUE_EMAIL_RELAY_NAME \
  --cwd MACHINE_RELATIVE_WORKDIR -- /ABS/TRUSTED/python3 -I -B \
  /ABS/PINNED/email-relay-bootstrap.py /ABS/NEW_PRIVATE_DIRECTORY/bootstrap.json
```

The Python interpreter and bootstrap script are fixed trusted installation
artifacts, not external request arguments. Do not copy workload credentials into
any file. No credential or environment override is accepted by the manifest.
Read the returned actual ID via normal `agent show` and verify saved startup via
`agent admin startup show`; if either is unavailable, stop without recreating.
Bootstrap requires its exact workspace/machine and existing workload connection.
Native relay validates its own executable/CLI hashes and maintains normal Core
message durability and process exclusion.

The same verified Hub Agent API can then `preview_create` → `create` paused →
`bind_email` using the selected member/address. `preview_resume`/`resume` remain
subject to the existing gate and fences; do not change them as part of preparation.
`preview_email_setup` and delivery-journal recovery are human-owner-only, whereas
ordinary binding operations are available to verified workspace Agents. Do not
confuse those two scopes. When separately enabled, counterpart Core message →
relay → Hub durable thread → Treer's built-in email API supplies the notification;
member OAuth/thread reply → durable inbound queue → relay → fixed counterpart
completes the other direction. No old relay or arbitrary email shell command is
in this route. Existing UI, SQLite queue, idempotency, sent/unknown status and
pause/revocation checks are reused.

Focused bootstrap checks (no live create, lifecycle, Core mutation or mail):

```sh
TREER_SETUP_TEST_RELAY=/ABS/BUILT/treer-endpoint python3 -m unittest discover \
  -s apps/message-endpoint/tests -p test_email_relay_bootstrap.py -v
```
