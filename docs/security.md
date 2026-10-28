# Security Model

Treer's current supported tier is trusted or mostly trusted machines and
workspace members. It is not a safe multi-tenant execution sandbox.

**One OS account is one trust domain.** Multiple workspaces/Controllers under
the same account are mutually trusted: A's exec/Agent can read B's local
credential files and impersonate B. Unix 0700/0600 and Windows current-SID/SYSTEM
ACLs do not isolate processes belonging to that same user. Separate roots,
workspace credentials, Policy and loopback ports do not change this boundary.
Use independent OS users or containers for mutually untrusted workspaces.
Connect warns when another local enrollment filename exists; this is a
name-only existence check, not inspection of its credentials.

For environment-bound CLI identities, explicit `--workspace`/`--url` cannot
switch the bound workspace/Controller. Conflicting flags fail before discovery,
network requests or credential forwarding. These are accidental-misdirection
checks, not protection against a same-account process replacing its environment
or reading another account-owned installation. Independent Agent clients should
set `TREER_WORKSPACE_ID` and `TREER_AGENT_SERVER_URL` along with their credentials;
if either binding exists, an explicit override of a missing or different binding
fails closed. With neither binding, an independent Agent client may select its
connection explicitly: its credentials are sent to that explicit URL, which the
caller must trust. Flag-free use is unchanged. Errors never describe the
destination installation or its credentials.
This consistency guard applies only to managed identities or operators with an
environment-bound workspace/URL; a pure operator with no such binding is expected
to select its connection using explicit `--workspace`/`--url` and is not restricted
by this guard.

On Windows, local Named Pipes reject remote clients and use explicit current-user
and SYSTEM DACLs. Credential-bearing configuration and recovery files use the
same user boundary, with atomic file replacement. SID-scoped startup mutexes and
PID creation-time checks avoid duplicate Host starts and terminating a reused
PID. Job Objects provide process-tree cleanup, not resource isolation or an
untrusted-code sandbox; processes under the same user remain mutually trusted.
Treer on Windows assumes an account in the Administrators group. The installer
and the Controller classify the process token (elevated, UAC-limited
administrator, or standard) and refuse standard accounts with the fixed code
`windows_requires_administrator` before writing, enrolling or starting
anything; non-administrator layouts are not supported or partially supported.
Administrator membership is a product precondition, not a trust boundary: it
does not change the same-account trust domain described above.
The Windows installer checks hashes supplied by the same artifact origin and
matching binary build identities; this is not independent signature validation.
Use a trusted HTTPS Proxy/artifact origin for public installation.

Local Controller/Host framing preserves partial reads while process/output
events are delivered. Invalid lengths, decoding and request protocol versions
close the connection; payload bytes are never intentionally resynchronized as
new commands. This is protocol integrity within the existing local socket/pipe
trust boundary, not authentication of arbitrary same-account processes or a
sandbox. The regression tests use isolated in-memory connections, including a
fragmented malformed frame containing a nested restart request; no deployed
Host is used for adversarial validation.

## Supported Claims

Organization invitation acceptance is explicit, human-cookie-only and requires
the exact Console Origin. Agent, operator Bearer and service-audience credentials
cannot join an organization through that endpoint. Organization invitations grant
member only; existing membership roles are unchanged. Membership, invitation
consumption and acceptance audit commit together. Preview exposes only the named
organization/kind to someone already holding the invitation. Shared PostgreSQL
rate budgets limit preview/accept to 60 attempts per TCP peer IP and 600 globally
per minute. Forwarded IP headers are not trusted; users behind one reverse proxy
may share a peer budget, and global exhaustion can deny invitation use temporarily.

OAuth invitation/consent state stores only a hash of the invitation, is bound to
an HttpOnly browser cookie, expires after ten minutes and is consumed once.
Existing OAuth users do not autojoin; their pending invitation is bound to the
new human session and must be explicitly accepted. The POST start body keeps
invitation tokens out of navigation and IdP/callback URLs. Pending IDs/tokens
must not be put in browser storage. The original invitation table still stores
bearer tokens in plaintext and has no invitation expiry field: database access
can expose unused invitations. Hashing the short-lived OAuth/pending records is
not a claim that all invitation storage is hashed. In invitation-only deployments,
an org invite without `join_organization:true` is rejected as `invalid_invitation`
without creating an account or consuming the invite, for both password and new
OAuth accounts. Open-registration deployments may still create a personal account
without joining or consuming the org invite; that is open registration, not
single-use invitation admission. Failure to retain pending state after successful
OAuth login does not invalidate the session: only a fixed warning/code is logged
at the handoff, and the user must reopen the invitation to join.
Mixed old/new Proxy replicas are unsafe for
the new consent contract; all replicas must upgrade before enabling the new web
flow. Invitation payloads and tokens are excluded from acceptance/pending audit.

Managed App startup edits require `agent.startup.manage` on the stable runtime
ID plus ordinary restart permissions. Controller `app.update.v1` preflight
validates root-relative directories and recognized Treer credentials before the
Proxy saves launch fields. Revision and generation fencing serialize updates
with lifecycle operations; identity, workload credentials and ingress records
are not replaced. Startup specifications are plaintext, not secret storage:
arbitrary third-party secrets cannot be detected reliably. Use private App
configuration. Audit events identify the App and routing metadata, never argv
or configuration contents. Failed launch retains the newly saved specification.

- Controller hot updates authenticate the signed release channel and manifest
  against a preconfigured local Ed25519 public key, then verify both binary
  lengths and SHA256 hashes before executing either binary, including `--help`.
  Missing signatures, keys, invalid metadata, or corrupt binaries fail closed.
  Explicit and existing local trust keys take precedence; failures reading or
  parsing them never silently select another key. Only the exact official
  distributor may use a key pinned by its trusted installation/package when
  the implicit local key is absent. Custom distributors require separate local
  trust configuration. The trust key is never obtained from the channel or
  manifest being verified, and a package without a pinned official key refuses
  the default update. This does not cover
  bootstrap installers, Windows in-place updates, or replay/downgrade protection.

  Internal Windows confirmed-plan admission also matches the fixed private local
  catalog and complete operation scope against the four actual signed/held input
  files and selected runtime. Reservation rereads the catalog before claim;
  changed bytes or stale plans refuse. This is not caller authorization, catalog
  CAS, measured running-image provenance or production Windows updater enablement.
  The runtime's loaded StartupStore remains the separate state-root authority.

  The internal same-Holding worker publication continuation additionally requires
  explicit sender image release, the original released custody, normal Trust,
  confirmed catalog, selected runtime and loaded writer fence. The receiver
  retains its independent job/lease/claim/backup/state guards, records an
  exclusive started phase before either rename, and verifies physical identities
  plus all four complete hashes before recording publication. Two renames are
  not pair-atomic: a second-rename failure leaves a mixed pair and unknown with
  guards retained for the original bounded lifetime; it never retries or guesses
  rollback. Candidate write objects are replaced through retained identity
  bridges by restrictive read objects, then rehashed before acceptance. File
  publication and terminal PE execution are not Controller restart, health,
  startup recovery, durable terminal outcome or claim retirement. The public
  Windows capability remains off.

  The internal worker-side input receiver verifies both signatures with an
  independently supplied trust key and all four whole-image digests under
  independently opened physical pins. Its state snapshot is compared with the
  original selected-runtime sender's loaded-store observation, including raw
  digest/file identity and denial fences; this is not independently established
  child-side runtime authorization or a freeze against subsequent writes.
  The bounded wire offer includes no trust key or raw startup credentials.
  A distinct received receipt binds the entire offer and fresh challenge only
  after verification, while receiver handles and the sender reservation remain
  held. It is not a sender-release permit, lifecycle authority, readiness,
  durable replay protection or proof of worker survival after creator exit.
  Production worker argv/coordinator wiring and capability remain unchanged.

  The internal Windows worker bootstrap additionally has a one-use, bounded
  binding exchange on the original Child's private transport. Its fixed ACK
  agrees to an operation, fresh challenge and digest of the retained selected
  runtime/input evidence. Child-side creator-handle checking proves transport
  origin only; echoing the context is not independent runtime authorization.
  Neither an ACK nor its wire data constructs admission, transfers locks,
  permits lifecycle commands or proves readiness. The Windows private entry can
  only complete this exchange and exit, before ordinary configuration or Host
  initialization. The transaction coordinator and capability remain unwired;
  normal identity/Policy and signed admission are still required. A zero exit
  after ACK is not update success or readiness. The original Child owner can
  separately observe zero exit within one three-second completion budget only
  after a successful ACK. Timeout, cancellation or error consumes that attempt
  without granting authority or releasing its reservation; independent teardown
  remains one-shot and unknown Drop does not prove exit.

  A separate internal `ObserveController` read asks the stable Host supervisor
  for its original live Child's epoch/instance/PID. The instance UUID changes on
  each successful spawn, even if a PID is later reused. This is an observation,
  not independently authenticated mapped-image evidence or a restart token.
  It neither extends the local Host socket's OS-owner trust boundary nor binds
  a workspace/operation/selected runtime by itself. The updater has not wired
  it to a production restart or enabled capability. The existing unconditional
  Host restart command is not made conditional by this addition.
  The internal selected-Controller reader uses only that retained runtime's
  existing HostClient and checks its operation/workspace/server/runtime scope
  before sending and after receiving. It requires the synced Host epoch, this
  Controller process PID and a canonical UUIDv4/RFC instance. Unexpected wire
  responses or Host errors produce a fixed refusal, with no restart fallback.
  The returned identity is still ordinary observation data: it cannot recreate
  the selected runtime/state authority or authorize a later lifecycle mutation.

  The separate conditional Host restart mutation compares that observation
  against the original live Child at execution time. It uses the existing
  local Host socket's OS-owner authorization, not possession of the observation
  as a credential. Its bounded operation cache binds the expected identity and
  refuses collisions with the legacy unconditional command. After signalling,
  uncertainty latches before any further action: no automatic supervision or
  conditional retry occurs until explicit recovery. This latch and cache are
  in memory, not a durable cross-Host-restart fence. Explicit legacy operator
  restart remains available; an updater must not fall back to it. A successful
  replacement resumes normal existing supervision and proves creation only,
  not health, workload continuity or update completion. No selected-runtime
  mutation admission or production coordinator is connected by this primitive.

  The internal selected-state request wrapper performs fresh same-HostClient
  observation and binds the conditional mutation's operation UUID and expected
  identity itself, never accepting a caller-supplied observation. Foreign
  runtime/operation/workspace/server refuses before consumption; a valid call
  consumes its in-memory attempt before any await. Cancellation, bad response
  and transport failure never trigger an unconditional fallback or internal
  retry. The wrapper does not grant Policy permission, transfer authority to a
  worker or survive creator exit. Existing normal admission must precede any
  future product entry; no such entry or capability is enabled here.

  Child-side creator pipe admission now retains a non-inheritable duplicate of
  the inherited process handle with query/synchronize rights only. The borrowed
  standard handle is never closed or owned by this constructor. The retained
  handle remains stable across stdin replacement and transport loss. Creator
  exit uses the process handle's wait state, not exit-code 259 or a reopened PID.
  A consuming three-second wait closes the pipe first and refuses a live creator
  at deadline; cancellation drops custody. Neither exit nor pipe EOF authorizes
  any mutation. This adds no worker lock/pin transfer, Job policy/breakaway,
  independent Controller/Policy authorization or automatic reconnect/retry.

  Returning the retained connection after a validated binding ACK preserves
  custody only. The child still echoes opaque runtime context, rather than
  independently authorizing it. Its retained-ACK helper has no mutation dispatch,
  command parser, lock transfer or reconnect. The terminal bootstrap explicitly
  drops this connection on success; its finite refusal/no normal-startup fallback
  is unchanged. An eventual worker must separately establish admitted input,
  lease/pin ownership, authorization and survival before using any lifecycle API.

  The fixed private worker's stderr slot now explicitly carries a reduced-rights
  duplicate of the SAME held installation lock file (read attributes and
  synchronize only), not log output. The child does not deserialize a handle
  number or reopen a lock pathname. Retention requires an already verified live
  creator connection and the expected physical lock-file identity, with live
  checks before/after. That expected identity must eventually come from admitted
  operation handoff; matching a caller-supplied tuple alone is not authority.
  Nonfile/changed-identity inputs refuse. The borrowed standard handle is not
  closed by the receiver, so no ACK, transport cancellation or retained-object
  drop promises lock release before child exit. This is conservative reference
  lifetime only: it neither transfers ancestor/image/state pins nor retires a
  claim, permits replay, authorizes restart or enables a production worker.

  The new internal lock-binding variant obtains the expected lock-file identity
  from the retained reservation, binds it to the selected runtime/input context,
  operation and fresh challenge, and accepts only the exact separate-role ACK.
  Child ACK requires a matching actual inherited lock and the private bootstrap
  operation, not merely echoing a digest. Both sides have a three-second total
  budget; child checks elapsed time after synchronous lock inspection and before
  returning success. A late/partial/failed/cancelled exchange consumes the same
  latch as the legacy binding exchange, with no downgrade/reconnect. Parent
  scope is rechecked after ACK and its reservation/pins/claim remain held. This
  proves limited context/lock-reference agreement, not independent child Policy,
  signed-input admission, all-pins ownership transfer, restart/health authority
  or production readiness. No normal startup or terminal-bootstrap entry changes.

  `PinnedPair::open_matching` is a separate mechanical receiver primitive: it
  independently opens the existing private fixed-NTFS directory, physical
  ancestors and two fixed read-only images, then compares all three physical
  identities to sender observations. Sender pins must remain held throughout;
  this is not a way to adopt stale observations after a release/reopen gap.
  The public identity tuple is not an authenticated descriptor, signed-content
  proof, state-root authority or permission to release the sender. The internal
  signed receiver takes no caller tuple/path: only the original reservation can
  mint its opaque borrowed descriptor after full selected-runtime matching.
  Successful receiver pins retain that borrow, so sender originals cannot be
  released while the result lives. The separately supplied trusted key must
  match the original admission key; no manifest-provided key is used. Both
  signatures and four whole held images are rechecked, with every original
  binding digest compared rather than replaced by observed values. Failure
  drops new pins and emits only a static refusal; its consumed latch prevents
  another receiver attempt, fixed worker spawn or parent pair publication.
  This is an in-process source boundary, not child-side IPC authentication,
  caller Policy admission or a permission transfer. Original runtime state and
  fences remain owned by the sender. The consuming receiver-state stage gets
  its directory only from that loaded runtime authority and independently pins
  it before comparing fixed-file snapshots by physical ID and full raw digest.
  It reuses complete v1/v2 scope/denial-fence validation, retains no raw entries
  or credentials, and exposes no restore method. A changed/missing/invalid
  snapshot or read error permanently invalidates its read-only observation.
  Directory pins do not freeze legitimate StartupStore file replacements;
  successful snapshot inspection is not a concurrent-write CAS or permission to
  restore stale state. Cross-process authority, explicit custody transfer and
  original-owner release are still required for a production worker handoff.
  Success/creator exit never grants them.

  The private input-receiver terminal entry separately resolves trust before
  reading any offer. Its creator reuses the normal release resolver, preserving
  explicit/local-key priority and fail-closed errors; only a missing local key
  on the exact official base selects the bundled official key. The child uses
  that fixed bundle or the selected absolute public-key path, never a key from
  metadata, a receipt or the offer. Key reads are limited to 16 KiB. The launch
  preflight requires the configured key to match the original admission and
  rejects configuration changes before spawning. This remains a local OS-owner
  trust boundary, not independent child Policy or protection from that owner
  changing configuration. All creation failures return original sender custody;
  the consumed creation attempt is not a retry token. Successful reception and
  a terminal zero exit do not release sender pins/lease/claim or grant mutation,
  restart, recovery or readiness. Product activation remains disabled.

  The holding receiver uses that same independent trust and signed-input
  verification. Its coordinator consumes already confirmed installation handles
  and the selected loaded runtime; it does not authenticate a new caller or
  replace upstream Policy. Inspect/close frames bind the original whole-offer
  receipt context and exact monotone sequence, with distinct request/reply roles.
  Successful inspection is only a point-in-time startup/fence recheck. Explicit
  close and observed original Child zero exit move the original sender guards
  back without dropping them, retiring claim or granting mutation. ACK or creator
  exit alone never permits sender release. Errors/cancellation poison the
  session before await; no automatic reconnect/replay. Receiver custody ends on
  session exit, but sender guards remain owned by the live coordinator. Unknown
  owner destruction retains the existing best-effort teardown limitation, not
  proof of child exit or continued guard ownership; durable claim still fences
  another operation. No public capability or production mutation route is added.

  Windows saved-installation preparation reads only the normal service config
  root, with fixed safe JSON names, 256 KiB per file, 1024 directory entries and
  16 MiB aggregate config input. Minimal projections omit credentials from
  diagnostics; original private single-link files and ancestor handles remain
  held. Host/controller path, socket and root bindings, exact workspace/server,
  selected runtime machine root and physical pair identities are checked.
  Shared physical images require the catalog's existing permission plus the
  exact confirmed affected-server list. Snapshot identity is not process
  authority, a freeze of future enrollments, or permission inferred from privacy
  or same-volume paths. Enrollment-name changes poison phase-boundary checks.
  Config/catalog writers may be refused while these read pins are held.

  Staging uses verified normal-config trust, both signed documents and complete
  artifact bytes, never an offer-selected key. Only a fresh exclusive operation
  directory under runtime-owned state is created. After writing and flushing,
  strict pinned files are reopened; this initial unadmitted staging boundary
  makes no continuous-file-custody claim. Existing confirmed admission verifies
  all held bytes again before claim. Errors keep partial staging/claim evidence,
  with no automatic cleanup, overwrite, adoption or replay. Closed-session
  custody returns the original reservation WITH saved-config/installation pins;
  ACK/creator exit never releases them. This is internal preparation only, not
  enabled product dispatch, child mutation authorization or a complete updater.

  The normal release download seam retains the actual four signed metadata
  bodies and complete verified binary inputs using the existing release
  consumer, not a second downloader or newly manufactured signatures. Local
  trust resolves before network access; preparation reloads it and verifies the
  same bytes before staging. No proxy or redirect fallback is permitted. Connect
  is bounded to 10 seconds, each request and the full download to 120 seconds;
  existing 1 MiB metadata and signed artifact-size/128 MiB binary bounds apply.
  Download error/cancellation cannot leave a claim or stage because preparation
  has not begun. Errors are static at the preparation boundary, with no automatic
  retry or unsigned fallback. Retained HTTP documents are input evidence only,
  never lifecycle authority or a sender-release permit.

  The Windows confirmed-command consumer lives on the existing authenticated
  Proxy client, not a new login, Policy or confirmation route. Production command
  handling retains the capability check, currently OFF. One full binding owns
  a detached task and pinned normal local catalog across download and receiver
  closure; exact repeats observe, conflicts refuse, and error/cancellation does
  not reset the slot. Explicit signed-release settings are local configuration,
  not offer-selected trust. The exact shared-server list in that configuration
  is a local operator confirmation; it does not prove the current browser
  confirmed those impacts. Enabling the product still requires the complete
  transaction and confirmation contract. All reports after finite session close
  remain Unconfirmed, including successful input reception. The owner retains
  original custody without a release permit. Process/client destruction is not
  an explicit custody transfer or proof that a child exited; the retained claim
  remains a denial fence. There is no durable owner recovery/adoption API here.

  The same Holding owner may explicitly grant one limited backup-preparation
  write. Its internal non-serializable permit borrows the original confirmed
  reservation/runtime; fixed catalog, operation, claim and configured trust are
  checked before sending a distinct full-context/sequence-bound command.
  Inspection, initial reception, close ACK and creator exit are not that grant.
  Receiver-created backups use only the original independently pinned target,
  old images and runtime-derived raw state. The fixed child directory and each
  output/evidence file are exclusive; existing or partial outputs are never
  adopted, repaired or retried. Whole-file hashes/raw snapshot fences and evidence
  are read back before acknowledgement, with read-only backup handles retained
  alongside input pins and the inherited lock. State comparison is point-in-time,
  not StartupStore CAS or a freeze of concurrent writes. Failure before a reply
  can leave backup files, and receiver custody ends when its session exits;
  sender guards and durable claim still do not become a release permit. This
  grant adds no installed-image/startup overwrite, cross-creator lifetime,
  independent child Policy, restart authority, or public capability.

  A separate internal continuation after that explicit backup grant can perform
  one synchronous compare/write on the selected runtime's actual loaded
  StartupStore. Its non-wire token retains the same Arc, revision, operation and
  original physical/raw snapshot. Normal writers and CAS share the same mutex
  through fence validation, existing persistence, readback and memory commit.
  Replacement entries must retain workspace/server scope and all existing v2
  deletion/denial fences. Unknown persistence/readback fences normal writers on
  that instance; neither reloading a path nor replaying a token reconciles it.
  The same Holding worker independently checks the new physical/raw snapshot and
  original backup fences before a sequenced acceptance. Both sender custody and
  the old backup remain held. Acceptance is not a release or lifetime grant.
  This is loaded-instance CAS, not OS-wide CAS, a freeze of later writers or
  durable cross-creator recovery. The finite public consumer remains read-only
  and public Windows capability remains OFF.

  An explicit fenced CAS continuation prepares mutation by installing a
  non-serializable operation owner on that SAME loaded Store under its mutex
  before persistence can have effects. Successful readback retains the owner;
  ordinary cloned writers cannot change the accepted startup state during later
  Holding/candidate work. The coordinator retains the committed owner before
  awaiting worker acceptance. Unknown/error/cancel/close/Drop do not clear it,
  and a changed physical/raw snapshot permanently poisons it. Wrong operation
  queries refuse without poisoning the legitimate owner. The existing ordinary
  CAS remains unfenced. The new fence is in memory only, not a denial fence in
  startup JSON, durable takeover, OS-wide exclusion or a sender-release token.
  A separately loaded Store/process is outside its scope; no reload-by-path or
  automatic recovery is exposed to reconcile this operation. Public Windows
  capability remains OFF pending durable transfer and outcome acceptance.

  A distinct SAME Holding transfer continuation requires the original confirmed
  reservation/normal Trust, operation-owned loaded writer fence and verified
  backup plus all new/restore candidates. Its complete strict transfer contract
  binds operation/workspace/server/installation/digests, whole-offer context and
  challenge, current raw startup snapshot and a maximum 120-second deadline.
  The receiver writes one fixed exclusive job record, flushes and reads it back,
  retains a readonly duplicate of that SAME object (whose sharing denies new
  writers/deleters), and keeps independent job/lease/claim/target/state/backup/
  candidate custody before ACK. The sender's readonly observer shares WRITE to
  coexist with the receiver object's original access; its immutability depends
  on that live receiver object, not the serialized record after owner exit.
  Records are denial evidence only; no restart/adoption/replay constructor exists.

  An ACK does not drop original images. A separate explicit synchronous sender
  release rechecks loaded owner, selected runtime/plan/Trust, original live Child
  and exact durable record, then consumes only image pins, including saved
  inventory duplicates. It retains original parents/config snapshots/job/lease/
  state. The receiver may hold custody after connection loss or creator exit
  until its accepted deadline; EOF never authorizes mutation or reconnect.
  Private worker creation preserves enclosing Job policy and stable Host tree
  cleanup. Original Child liveness is checked before/after record verification;
  bounded custody does not promise survival of Job/Host/OS termination, reboot
  or power loss. No Job settings or Host process are changed.
  Partial/unknown records are preserved and never adopted or retried; expiry/
  close never clears the loaded writer fence or retires claim. Pair publication,
  Controller-only restart/health, rollback, durable outcome and safe retirement
  remain separate. No public capability, production dispatch or Host action is
  enabled by this finite custody continuation.

  Receiver custody includes an independently opened fixed installation claim
  after normal configured-trust whole-input authentication. Core supplies the
  exact canonical full operation binding; Platform retains a private regular
  single-link read-only handle with no write/delete sharing and bounded byte
  comparison. Receipt is sent only after that check, and later Holding checks
  reread the same file handle. A matching durable claim remains a denial fence,
  not independent Policy, takeover permission or sender release. No claim
  creation, overwrite, recovery or retirement is exposed by this reader.

  Receiver job custody duplicates only the original sender's job lock into its
  original held Child, with read-attributes/synchronize rights and no inheritance
  flag, PID lookup or write/delete rights. The borrowed child-local handle value
  is duplicated locally rather than adopted or closed. After independent signed
  input verification, physical directory/lock IDs and the complete canonical
  intent must match; the receiver retains private ancestor and receipt pins and
  rechecks the same receipt during Holding. Offer locators/receipt hashes do not
  grant Policy or mutation authority. A failed/unknown offer leaves any remote
  reference until child exit; remote-close could otherwise target a reused handle.
  Sender originals remain held. Creator liveness, finite session and unknown
  no-replay constraints remain; this does not permit creator exit or sender release.

  A distinct, explicitly requested fixed-candidate grant on the SAME Holding
  connection follows backup preparation. Its non-wire constructor rechecks the
  original confirmed/runtime/claim contract and normal trust, not offer-selected
  keys or creator identity as authorization. Worker creation is restricted to
  four fixed CREATE_NEW candidate names under the independently pinned saved
  installation. New images come from held signed inputs; restoration images
  come from independently verified backups. Whole hashes, flush and retained
  physical handles precede ACK; existing names and partial outcomes refuse
  replay. Reduced lock custody and all original image/state/job/claim pins stay
  held. This is candidate-file mutation only: no installed-image rename,
  publication, sender release, cross-creator lifetime, ready or update outcome.
  Close does not clean/reuse candidates, and the public consumer/capability
  behavior remains unchanged.

- Machines enroll once, then authenticate with a workspace-bound credential.
- Managed Agents have separate workload credentials, verified by both the
  Controller and Proxy and bound to one Agent, machine, and workspace.
- Controller snapshots and incremental Agent events are checked against durable
  Agent credential bindings before changing routing state. Nested inventory must
  match the authenticated machine's workspace and server ID. Unknown legacy
  Agents without durable credentials remain supported; they are not proof of a
  durable machine binding. Command responses must match their pending target
  machine and arrive through a current machine connection.
- An Agent can register or clear only its own Agent Interface Server. The
  Controller verifies the registered loopback manifest before advertising it and
  never forwards the workload credential to the interface. Its local recovery
  cache contains only a process-bound descriptor and is revalidated before use.
- Agent-authenticated service mutations are bound to the Agent's own machine
  and Agent ID; cross-owner requests fail before Policy can grant them.
- Local operator requests use a private Controller credential that is not
  injected into managed Agent environments.
- Service tokens are short-lived and audience-bound. Human App token
  verification rechecks current workspace membership and service existence.
- Core Message authorizes send, read, receive, ack, and operator import
  separately. Context edges do not expand visibility.
- Message bodies stay out of ordinary logs, audit payloads, domain events, and
  outbox envelopes. They remain plaintext in Core PostgreSQL.
- Organization and workspace management plus lifecycle mutations record
  body-free audit events. Several lifecycle routes log and continue when audit
  persistence fails; audit completeness and tamper-proof storage are not promised.
- Machine exec and file upload require current workspace access and distinct
  `machine.exec` / `machine.file.write` Policy actions. Their paths are confined
  beneath the enrolled Host root, and audit records omit command arguments,
  output, and file content.
- Chunked, resumable browser upload (#166; capability `machine.upload.v3`,
  advertised only where the Controller supports it, not on Windows until #171;
  the Controller's command dispatch refuses any command whose required
  capability it does not advertise, so the unvalidated Windows path is
  unreachable while the legacy route stays;
  routes `POST .../machines/{id}/uploads`, `GET .../uploads`,
  `GET|DELETE .../uploads/{upload_id}`, `PUT .../uploads/{upload_id}/parts`,
  `POST .../uploads/{upload_id}/commit`). The Railway edge ends any request
  body after five minutes, so a file travels as parts of at most 8 MiB (at
  least 1 MiB except the last, at most 160 parts), each its own short request.
  Every route re-authenticates, requires the exact Console origin (the routes
  are Console-class), re-evaluates `machine.file.write` on the machine, and
  must come from the subject that began the upload: anyone else, including
  another member of the workspace, gets `404 upload_not_found`. Begin requires
  an `Idempotency-Key` (32 to 128 characters, at least 128 random bits) scoped
  to subject, workspace and machine: a replay with identical parameters
  returns the same upload without consuming quota or a Controller slot, a
  replay with different parameters is `409 upload_begin_conflict`. The upload
  id is 256 random bits and only a handle. Quotas are counted in PostgreSQL
  so they hold across Proxy instances: two active uploads per user, eight per
  workspace, eight per machine, one gigabyte of declared size per machine and
  ten begins per minute per user (`429` with `Retry-After`); a Proxy instance
  additionally streams at most 32 part bodies at once and rate-limits parts
  (30 per minute) and status polls (60 per minute) per upload. Everything
  that can refuse a request is decided before its body is read (exactly one
  numeric `Content-Length`, no `Transfer-Encoding`, part shape, offset against
  the committed offset, the lease), and refusals answer `Connection: close`.
  Parts are sequential and append-only: a part must start at the committed
  offset (`409 upload_offset_gap` with the committed offset in the hint
  otherwise), carries `X-Treer-Part-Sha256`, is hashed by the Proxy while it
  streams in 192 KiB chunks with two commands in flight (about one megabyte of
  Proxy memory per part), and is closed with `UploadPartCommit`, where the
  Controller verifies its own hash of the written bytes before the committed
  offset advances; a failed part is truncated back to the committed offset and
  a retry rewrites from there; an identical committed part replays
  idempotently without a body, a conflicting one is `409 upload_part_conflict`.
  One part is in flight per upload across instances through a database lease
  (`409 upload_busy`) that is released when the request ends for any reason,
  including a dropped connection; a part has 30 s idle and 120 s total.
  Commit requires every byte committed, runs as a detached task that completes
  even if the client disconnects, and is idempotent for ten minutes; the
  Controller publishes only when written, committed and declared sizes agree,
  re-verifies the destination parent through the directory descriptor it held
  since begin (platform `Root`/`StagedFile`, the CLI transfer boundary), and
  publishes atomically without replacing when overwrite is off (link + unlink;
  an existing file is `machine_file_exists`). Status reconciles an active row
  against the Controller (`UploadResult`): a Controller ahead of the row after
  a lost response is adopted, a committing row whose receipt the Controller
  holds is finalized, so a client resumes from the Controller's truth. Before
  publishing, the Controller flushes the staging file, checks that its length
  equals the acknowledged byte count and re-reads it through the held
  directory descriptor to compare its SHA-256 with the acknowledged digest,
  refusing (`machine_upload_content_mismatch`, staging kept for abort) rather
  than publishing a file whose receipt would not describe it; a failed chunk
  write rewinds the staging file to the committed offset, and every part
  commit checks the on-disk length. Staging files are owner-only (`0600`),
  live under a recorded random name in the validated destination directory
  (visible there while the upload is active) and are removed on abort, hash or
  size failure, and by the Controller reaper (one hour idle, six hours
  lifetime); a restarted Controller sweeps the staging names it recorded at
  startup. The Controller keeps committed receipts for 24 hours so a Proxy that
  died between publish and the row update recovers the result instead of
  reporting failure. A row becomes `failed` only on a definitive Controller
  refusal or when the Controller has neither staging nor receipt for an active
  upload (`upload_lost`, after a Controller restart); a committing row with no
  Controller evidence stays committing and ends at the six-hour lifetime as
  `failed` with `failure_code outcome_unknown`, which the Console shows as an
  unconfirmed outcome, never as a definitive failure. Audit: `machine.file.upload_begun`,
  `machine.file.upload_aborted` (fixed reason) and `machine.file.uploaded`
  (path, bytes, actor, the Controller's SHA-256), metadata only; parts are not
  audited. The legacy base64 route keeps its 16 MiB cap; uploads begun without
  a declared size accept only legacy appends and declared-size uploads accept
  only offset-bearing chunks (`machine_upload_mode_mismatch`). No new
  filesystem authority: destination validation beneath the Host root and the
  single-component file-name rule are unchanged.
- Agent private state (attachments) is never placed where tools scan or
  commit: the Controller refuses a private directory that lies in any
  version-controlled tree (a VCS marker of any file type at the directory or
  any ancestor up to the filesystem root, `lstat` only, fail closed, regardless
  of the cwd) and a private directory below or equal to the Agent's cwd unless
  the relative path starts with a dot directory; cwd = HOME with hidden `~/.local/state` is
  allowed, a repository containing the state directory is refused with a fixed
  hint, and the agent-private root carries `.gitignore`/`.ignore` as defence in
  depth. Launchers mirror the predicate.
- When the Proxy serves the browser App itself (`TREER_APP_DIST_DIR`), it does
  so only for the App public URL's host, only from files inside the canonical
  build directory, never for reserved Core prefixes, with the nginx image's
  header set; and if the App shares the Proxy origin, embedded App interfaces
  and virtual-host tunnels are refused so App content never runs on the
  Console origin.
- Provider CLI installation runs only recipes embedded in the Core release,
  never browser argv, user-editable tables or remote recipe URLs; see
  [Provider CLI installation](#provider-cli-installation).
- CLI file transfer uses separate `machine.file.read` and `machine.file.write`
  actions on every chunk and commit, with caller/path/generation-bound handles.
  Treer private state is hard-denied by reserved namespace, physical-parent/name
  reservations (including absent files), and opened object identity, independent
  of Policy. The Controller passes the whole canonical Treer state directory
  (`service::state_dir()`: startup stores, private state, provider CLI results,
  service logs, machine identity and any future per-server file) plus its
  service configuration as protected paths, so an enrolled root that contains
  that directory (root = HOME with `~/.local/state/treer`) cannot read or write
  below it through file transfer under any Policy; sibling directories stay
  ordinary transfer targets. A root equal to or inside the state directory is
  refused with a fixed error (`root_inside_treer_state_directory`) at
  enrollment, root relocation and Controller start, and a Controller whose
  state directory cannot be resolved does not start. Commit refreshes these checks after atomic Core file replacements.
  Reserved Core names conservatively fold Unicode normalization and case; if a
  Core directory is absent, its first missing component is reserved too.
  Upload destinations are validated before recording a durable staging intent,
  then checked again when staging. Recovery discards legacy intents only when
  a syntactically valid path matches the shared reserved namespace rule, without
  reading or deleting anything at that path. Its warning contains only a fixed
  code and count. Other corrupt metadata or unsafe cleanup remains fail-closed.
  The current normalization/case rule also applies to old records: a newly
  reserved Unicode alias may leave old staging untouched rather than risk
  deleting private state.
  If stage-failure cleanup or its journal rewrite cannot be confirmed, that
  Controller instance rejects further transfers until replaced; later requests
  cannot overwrite the unresolved intent from its in-memory registry.
  Temporary files remain private until digest
  verification; no-replace publication is atomic and overwrite is explicit.
  Portable paths reject Windows drive/UNC/device/ADS/short-name aliases too.
  A streamed SHA-256 is not a snapshot or an authenticity signature. Audited
  file hashes can reveal equality and permit guessing low-entropy content;
  restrict audit access accordingly. File request attributes do not currently
  provide filesystem alias-aware path selectors. Same-account OS access and
  ancestor rename races are outside the isolation guarantee.
- The original Agent startup register/read/clear interface remains self-only.
  The Proxy applies distinct `agent.startup.read` and `agent.startup.manage`
  actions, and a restored process is launched only while its durable Agent
  credential remains active and bound to the same machine.
- The separate admin startup read/set routes evaluate `agent.startup.manage`
  on the target Agent, including cross-Agent callers. Browser users require
  workspace access and are evaluated as humans; local operators use their
  authenticated machine identity. Replace-and-restart additionally requires
  the existing `agent.stop` on the Agent and `agent.create` on its machine.
  App runtime edits are rejected in favor of the App deployment configuration.
  Startup management bodies and responses contain no credential field; known
  Treer workload/operator credential literals in new specifications are rejected. Audit events
  record actor/target IDs, never the command, arguments, or credentials.

## Overview pins

Workspace pins (#140) are shared state: any member who passes the Tree manage
rule can pin, unpin or displace others' pins within the 50-item limit, and the
audit log (`tree.pin.create`, `tree.pin.delete`, actor, kind and stable ID
only) records who did what. Reading needs the Tree read rule, so a member who
cannot read the Tree cannot enumerate pinned IDs through pins either; the
response and the `workspace.pins.changed` signal carry no names or paths, and
the signal grants nothing by itself (clients re-read `GET /pins`, which
authorises). `can_pin` is a hint computed from the same rule as the write
routes and false whenever Policy denies or fails; the write routes authorise
independently. The workspace event stream itself delivers every workspace
event to every member session without a per-subscriber Policy filter; that
pre-existing boundary is tracked in #141, not changed here.

## Provider CLI installation

Installing a provider CLI (Codex, Gemini; others are explicitly unsupported in
the first table) executes third-party code from the npm registry as the machine
account under a human's explicit confirmation. It is not a sandbox and does not
pin transitive dependencies: npm's own registry integrity checks apply to them.
What Core does promise:

- Only a human workspace session with Policy `machine.exec` on that machine can
  list, prepare, install, check or poll; App, service and machine credentials
  are refused. Prepare and install both re-check membership and Policy.
- The table is release bytes validated at Controller start (typed `npm-global`
  package, exact version, SHA-512 integrity, allowlisted registry, no free
  argv). The confirmation shows the exact plan that runs: `npm pack`, SHA-512
  verification of the single tarball against the recipe, `npm install --global`
  of that verified local tarball, prefix resolution and a version probe from
  the installed prefix's `bin`. A different fresh expansion (for example a
  moved npm) is refused as `provider_cli_plan_changed`; a table or Controller
  change since confirmation is refused, never substituted.
- npm runs from a Controller-owned `0700` job directory with a cleared
  environment: the machine exec allowlist minus `npm_config_*`, `NODE_OPTIONS`,
  `NODE_PATH`, proxy and TLS override variables, with `HOME` and temp inside
  the job directory, empty private `--userconfig`/`--globalconfig`, an explicit
  `--registry` and scope registry, `--ignore-scripts` on pack and (unless the
  recipe carries a reviewed lifecycle entry) on install. The user's `~/.npmrc`
  is never read, so a user-configured global prefix is ignored and the install
  lands in npm's default prefix for that Node; enterprise proxies are out of
  scope. Step 3 contacts the recipe's registry to resolve dependencies and
  writes the global prefix. A non-writable global prefix fails visibly; Core
  never elevates or edits PATH, so a successful install does not promise that
  Agent creation finds the binary: the probe reports the absolute path inside
  the prefix for a profile, or the user adds that `bin` directory to the
  service environment themselves.
- A lifecycle-enabled entry runs npm lifecycle scripts, so the table allows
  it only for a native-binary entry (#131): the one whose reviewed postinstall
  places a platform binary that the recipe pins. Such an entry carries the
  review (file hashes, the script map, which scripts npm runs, the expected
  closure of root plus exactly one native package), `omit_optional: false`,
  `binary_path` and one `native_binaries` pin per supported platform (native
  package, version, tarball SHA-512, member, size, SHA-256, unpacked size).
  Scripts without a pin or a pin without scripts invalidate the whole table.
  Its plan has nine steps: `node -p process.arch` from the allowlisted PATH
  must print the platform's native architecture (an x64 Node under Rosetta
  would select another native package; `provider_cli_node_arch_mismatch`,
  also refused at `prepare` from the observation), `npm pack` of the root and
  of the native package, SHA-512 verification of both tarballs, one
  `npm install --global` of both verified local tarballs with `--offline`, an
  empty private cache and every registry pinned to a closed port (nothing
  fetched during scripts can succeed, so a missing native fails closed), the
  prefix resolution, a read-only verify-file step and the version probe.
  Verify-file opens the canonical prefix once and every component below it
  with `O_NOFOLLOW`, requires each directory and the file to be owned by root
  or the account and not group/world-writable, the file to be regular with
  exactly the pinned size and SHA-256 (the postinstall's hard link makes it the
  same inode as the native package's member; nlink is not checked), and
  `<prefix>/bin/<binary>` to be a symlink resolving inside the prefix to that
  inode. Any mismatch is `provider_cli_binary_mismatch` with the failed check's
  category in the report, the probe never runs, and the report's
  `prefix_modified` tells the user to remove the package through the existing
  uninstall path; nothing retries. The probe's first line must carry
  `expected_version` as a complete token (`2.1.302`, not `2.1.3020` or
  `v2.1.302`) or the operation fails as `provider_cli_version_mismatch`; this
  rule applies to every supported entry. Native entries also require free
  space of 1 GiB plus twice the unpacked native size on the job and prefix
  volumes (`provider_cli_space_insufficient`) and may carry their own
  `deadline_seconds` within 60 to 900 seconds. Residuals: the native bytes
  are proven against the pin before anything runs them, but the reviewed
  postinstall itself runs as the account inside the scrubbed job environment;
  its placement is not atomic (verify-file catches partial results);
  same-account processes can race between verify-file and the probe; musl
  Linux is unsupported because npm would select a different native package.
  The unit tests prove the plan shape and the fail-closed checks against
  fixtures; that the offline install really satisfies the root's optional
  dependency with a single copy of the verified native package, refuses an
  injected native and uninstalls back to baseline is proven only by the
  release-owned sealed acceptance in a disposable environment.
  The packed tarballs are verified and installed by the same account in the
  same private directory; a same-account process replacing them between the
  verification and install steps is inside the trust domain Core already
  grants that account.
- The install step overwrites whatever lives in npm's global prefix, so the
  consent names what it replaces (#134). `prepare` first has the Controller
  observe the prefix with the same constraints as `check` (cleared
  environment, private `HOME`, process-group timeout and cleanup confirmation,
  bounded output; the probe executes the already-installed binary, which is
  third-party code the machine account already trusts). The observed state is
  `absent` only when the prefix resolved and the binary's `lstat` returned
  not-found, `installed` only when a regular file inside the prefix exited 0
  with a strict version on its first line (bounded numeric components, no
  leading zeros, bounded prerelease identifiers; the first matching token
  wins, so extra output cannot spoof a version into an upgrade), and
  `unknown` otherwise with the reason named (`provider_cli_prefix_unresolved`,
  `provider_cli_binary_outside_prefix`, `provider_cli_probe_failed`,
  `provider_cli_probe_timed_out`, `provider_cli_probe_output_truncated`,
  `provider_cli_version_unparsed`, `cleanup_unconfirmed`). Output that
  exceeded the retained tail has no trustworthy first line and never yields a
  version, so a crafted tail cannot turn a newer installation into an
  "upgrade" or a "noop". A probe whose descendants were not confirmed gone is
  not evidence at all: `prepare` refuses it (409
  `provider_cli_probe_unconfirmed`) and issues nothing. The action shown and bound is `install`, `replace`
  (relation `upgrade`, `downgrade`, `incomparable` or `unknown`) or `noop`;
  `noop` issues no token and nothing runs, an unobservable prefix refuses the
  prepare, and a spoofed probe that prints the pinned version only ever yields
  `noop`. A `downgrade`, `incomparable` or `unknown` relation must be
  acknowledged in the install request with exactly that relation; a missing or
  different value is 400 and leaves the token usable, so no client can confirm
  a downgrade blindly.
- The Controller re-derives the action from a fresh observation before any
  step and again immediately before the install step, under the operation's
  single five-minute deadline (each observation process gets at most the probe
  budget, never beyond that deadline, and nothing spawns once it has passed).
  Process facts are decided first: an observation whose descendants were not
  confirmed gone ends the operation `unknown` (`cleanup_unconfirmed`) with
  nothing executed after it, and a spent budget ends it timed out
  (`provider_cli_deadline`). Only then are hash, action and relation compared
  strictly; any difference (a prefix that moved, a binary that appeared,
  changed or became unreadable, an action that became `noop`) fails the
  operation as `provider_cli_state_changed` with nothing written to the prefix
  and the job directory removed. What remains is a time-of-check/time-of-use
  window between the second observation and npm writing the prefix: it is
  bounded by the remaining operation budget, not by any millisecond promise,
  and lies inside the same account's trust domain. Tokens minted before
  observation existed are never accepted, and a v1-only Controller is refused
  with `provider_cli_controller_outdated` rather than served unobserved.
- The Controller's observation is treated as input, not truth: the Proxy
  validates its shape before storing, auditing or showing it (canonical strict
  version, bounded control-free text, bounded absolute executable path, a
  probe code from the known set, and state/evidence agreement such as
  `installed` requiring a found binary and a clean probe); a malformed
  observation is a Controller error (502) that mints nothing. Every `prepare`
  is admitted against the per-minute and live-token ceilings before any
  Controller command, and counts whatever its outcome (`noop`, unobservable,
  refused recipe), so a session cannot make the machine run `npm prefix` and
  the probe without bound.
- The observed executable path (`<prefix>/bin/<binary>`, under the machine
  account's home) is returned only to the creator's `prepare` and `check`
  responses for display. Proxy rows, audit payloads, operation reports
  (`observed_before`) and logs carry the path-free summary: state, normalized
  version, presence and the observation hash.
- Confirmation tokens are 256-bit, stored hashed, expire after 120 seconds of
  database time, are consumed once by `UPDATE … RETURNING`, and are bound to
  workspace, machine, user, session, recipe triple, table hash, Controller
  build, plan hash, observation hash, action and relation. Expired, consumed,
  foreign and mismatched tokens share one 409. At most four live tokens and
  ten prepare attempts per minute per user and machine.
- One active installation per machine, enforced by a PostgreSQL partial unique
  index, with a five-minute deadline. Provider CLI commands are excluded from
  reconnect replay. Each step runs in its own Unix process group that is
  SIGKILLed at the deadline; descendants that leave the group but still hold
  the step's output pipes are detected and the operation ends `unknown`
  (`cleanup_unconfirmed`). Descendants that both detach and close those pipes
  are outside what Core can prove; `unknown` is never resolved by assumption,
  only by the machine's own later report.
  An `unknown` operation releases the Proxy's per-machine slot (the unique index
  covers `queued` and `running` only), while the Controller still refuses a
  second installation as long as its own job is active, so a job that is
  `unknown` to the Proxy but still running on the machine cannot be joined by
  a concurrent installation.
- Output never enters Proxy storage, audit or logs. Results stay in the
  Controller's private `0600` store for 24 hours (64 results; a full store
  refuses new installations rather than evicting). Audit rows record actor,
  machine, recipe triple, table hash, plan hash, action, relation, observation
  hash, observed state and version, status and exit code.
- Windows entries are all unsupported in this iteration; nothing executes
  there. Provider login is never performed or handled by Core.

## Machine connection ownership

Machine WebSocket registration is authenticated against the enrolled machine's
workspace and server ID before it may claim distributed ownership. A current
connection can recover an expired lease only through atomic create-if-absent;
renewal cannot overwrite a different connection's owner record. Lease revision
checks also prevent cleanup of an old socket from releasing its replacement's
ownership. A missing lease alone is not evidence of another Controller.

Enrollment binds a machine ID to one installation identity. A live machine
cannot be taken over by another installation, and other workspaces never
claim its ID. After the machine is deleted in the Console (its token and Agent
credentials revoked), a different installation enrolling with a valid
single-use key for that workspace receives a new machine ID; the revoked row
keeps its binding, so Policy rules, history and Agents of the deleted machine
are not inherited. The same installation reactivates its own row. Every
rewrite of an existing row holds its row lock and restates eligibility in a
conditional update, so concurrent claims never overwrite each other. Revoked
Agent credentials stay revoked for the old and the new machine ID. The
`machine.enroll` audit event records `replaced_revoked_machine` and the
previous machine ID, never installation identities or paths. On the
Controller, adopting the new ID is a journaled local transaction that keeps
private backups and never reads the previous machine's startup store or
private state.

## Process environment

Host/Controller do not inherit a caller's Treer workload identity or Agent UI
session. A shared platform environment filter removes Treer identity and
credential variables, `TREER_BIN`, `AGENT_UI_*`, `CODEX_UI_*`, and proxies whose
host is loopback (including localhost, IPv4 loopback and IPv6 ::1). Service
launchers filter before spawning; daemon entry points filter before creating
threads, including direct service-manager launches. On Unix a contaminated
daemon re-execs with the same PID so its initial OS-visible environment is
clean too. Controller hot replacement applies the same filter.

The PTY runtime filters the Host environment before applying each workload's
explicit environment, so restored Agents and Apps cannot inherit another
Agent's UI identity through the Host. Fresh workload credentials, per-Agent
proxy entries and the per-Agent private state directory are injected afterwards.
The filter also removes `TREER_AGENT_PRIVATE_STATE_DIR`, so no inherited or
caller-supplied value can redirect an Agent's private state. This is inheritance hygiene, not
provider-secret isolation: other user environment variables are retained and
same-account access remains possible. The Controller's control-plane HTTP
clients ignore environment proxies; external proxy variables are preserved for
user tools, but are not a control-plane transport configuration.

Machine `exec` uses a separate minimal OS environment allowlist. It carries no
`TREER_*`, `AGENT_UI_*` or `CODEX_UI_*` identity/configuration, environment proxy,
provider credential or loader-hook variables; it does not silently delegate the
calling Agent's workload identity. Windows keeps the native system/profile
baseline (including `SystemRoot`, `windir`, `USERPROFILE`, AppData, ProgramFiles,
ComSpec and PATH) with case-insensitive name matching. Agent/App launches instead
inherit the filtered Host baseline and receive their own fresh workload identity;
they never receive machine/enrollment/operator credential placeholders from the
Controller. Previously those names were inserted with empty values: names alone
were not evidence that the actual service secrets leaked.

For Agent/App process starts, restarts and recovery, the Controller resolves bare
executable names only against absolute directories in the final workload PATH.
Empty and relative entries are ignored; missing commands fail closed without
cwd or OS-default search. Relative executable paths such as `./run` are rejected;
explicit absolute commands retain their existing behavior. On Windows, bare-name
lookup accepts only `.exe`/`.com` and searches drive-qualified directories
(including drive verbatim paths), not UNC/device directories or caller-supplied PATHEXT
extensions. File symlinks must resolve to a regular executable target; Unix
targets require execute bits. The Host receives the resolved absolute command.
This prevents implicit cwd lookup, not replacement of executables by their OS
owner or a filesystem race: PATH directories and binaries must still be trusted.
It does not change machine exec's separate command/environment contract.

For Windows PowerShell 5.1, machine exec does not inherit `PSModulePath`.
Treer supplies only `System32/WindowsPowerShell/v1.0/Modules` under the Windows
installation directory returned by Win32, and also corrects SystemRoot/windir
from that query. It does not read an OS environment block or trust caller or
registry overrides for this directory. Symlink/reparse indirection in the
system module directory is rejected. Logs record source/count or an OS error
code, never directory values. No CurrentUser/Documents or
`ProgramFiles/WindowsPowerShell/Modules` path is supplied by Treer; automatically
loading third-party `Install-Module` installations is outside this default
baseline, and explicit extra-module configuration needs a separate design.
PowerShell itself can reconstruct module search paths during startup; this
initial environment is not a sandbox or a promise that native runtime state
cannot change. System-module integrity depends on OS directory protections,
not protection from administrators or same-account processes with write access.
In particular, Windows PowerShell 5.1 may add that account's default CurrentUser
and AllUsers module directories; Treer does not restrict that native auto-loading.
Use a separate OS account/container when those account-owned modules must be
isolated from Agents.

Windows PowerShell 5.1 automatic module loading can be slow without a usable
pre-generated module analysis cache. Hosted Windows 2022/2025 images showed
12–28s calls, while tested Windows 11 calls completed below one second; these
observations are not runtime latency guarantees. Production exec does not
inherit `PSModuleAnalysisCachePath` or create a Treer-managed analysis cache.
Only a CI-guarded Windows unit-test fixture uses the hosted image's cache;
that field/behavior is compiled out of production binaries. See the quality
guide for the test compensation and independent clean-machine evidence.

This reduces ambient environment exposure, not machine-account authority. Exec
still runs as the machine's OS user and can read that user's accessible files,
including local CLI configuration; omitting a credential from its environment
does not isolate it from that account. Restrict `machine.exec` using Policy and
separate OS accounts/containers where that authority is not appropriate.

- The administrator password is compared as fixed-length SHA-256 digests with a
  constant-time comparison, and sign-in for an unknown email verifies a decoy
  password hash. This reduces timing differences that reveal password length or
  account existence; it does not guarantee equal response time for arbitrary
  input (hash parameters, database latency and CPU queuing still vary).

- Password sign-in for humans and the administrator is limited to 10 attempts per
  account (or the single administrator) per 15 minutes across all Proxies. Attempts
  are counted atomically in PostgreSQL before password hashing, using the database
  clock and PostgreSQL `lower()` (the same normalization as the account lookup, so
  Unicode case variants share one budget); success clears the count, and expired
  rows are removed in bounded batches. Once the budget is spent a correct password
  is also refused, so guesses cannot be confirmed past the limit.
  **Known limitation:** the budget is per account, not per client. Without a
  trusted client-address model (the peer address behind a reverse proxy or load
  balancer is only the edge node), any anonymous client can make new sign-ins for
  a known account or the administrator fail for the rest of the window, and can
  create short-lived counter rows. Mitigations: existing user and administrator
  sessions are unaffected; operators recover with `treer-proxy
  --reset-login-attempts` (same `DATABASE_URL`; clears all counters and exits;
  not an HTTP route); restrict administrator login to trusted networks (a risk-reduction precondition, not a
  guarantee that the software prevents lockout; under a sustained flood first limit
  the failing traffic at a trusted ingress, then reset, then sign in). A trusted
  proxy-header model and per-source limits are follow-up work.
  Counting fails closed: a database error during reservation or clearing rejects
  the sign-in even with the right password (recovery of the database restores
  sign-in; no automated error-injection test exists). The limit applies once every
  Proxy replica runs this version (older replicas neither count nor enforce it),
  and the `login_attempts` table must be kept on rollback (older means the released
  Proxy without that table; unreleased intermediate table layouts are not migrated). It does not cover
  machine or other token endpoints, global CPU budgets, or OAuth/SSO. The
  administrator comparison and decoy hash reduce timing differences (see above)
  without guaranteeing equal response time for arbitrary input.

- The Controller refuses to open a service stream or probe to a link-local address
  (169.254.0.0/16, fe80::/10, including cloud instance-metadata endpoints) unless
  `TREER_ALLOW_LINK_LOCAL_SERVICES` is set on that machine, and connects to the
  address it checked. Human-registered services may still target any other host
  the machine can reach (private networks included); publishing such a service,
  especially with public access, exposes that host, so treat service creation as
  a network-reach permission and restrict it with Policy.

- The Proxy withholds the copyable Unix and Windows install commands when its
  configured public URL is cleartext HTTP on a non-loopback host (the commands
  download and run a script whose checksums come from the same origin, so a
  network attacker could replace both). Configure an HTTPS public URL. The
  `/install.sh` and `/install.ps1` endpoints and the connect command (which
  carries the enrollment key) are separate capabilities and are not blocked;
  `install.ps1` itself refuses cleartext non-loopback artifact origins.

## Unsupported Claims

Treer does not constrain Windows PowerShell's startup reconstruction or automatic
loading of that account's default CurrentUser/AllUsers modules. Its fixed exec
module environment is not a native-runtime sandbox.

The per-Agent private state directory (`agent-private/<server_id>/<agent_id>`
below the Treer state directory) is a same-user privacy boundary, not
isolation: its 0700 mode and the Controller's lstat/ownership checks keep other
OS users out, but every Agent and tool running as the machine account can read
and write every Agent's directory, including other Agents' processes. The
pathname checks before removal or measurement share that same-account trust
domain. The Windows check accepts as owner only this process's token user or token
owner (the SID the system assigns to objects the process creates; for an
elevated administrator that is the Administrators group) and requires a
protected DACL whose entries are all non-inherited plain allow entries for
the token user or LocalSystem; plain deny entries are parsed and rejected, and
unsupported ACE types (object, callback, audit, unknown) are never parsed
beyond their header and fail closed. Failures log a categorical report (owner class,
control flags, ACE type/inherited/principal class) without SIDs or paths.
Treer on Windows assumes an Administrators-group account: the Controller
classifies its own token (elevation type and BUILTIN\Administrators
membership, never names, paths or environment; a classification error counts
as non-administrator) and, for a standard account, reports the fixed code
`windows_requires_administrator` for private state (variable omitted,
reservations refused, status API) instead of a DACL verdict; the owner and ACE
rules are never relaxed for such accounts. On a hosted windows-2022 runner (an
elevated account whose token owner is the Administrators group) the earlier
owner-equals-user rule failed; the token-bound rule and the DACL negative
regressions (wide DACL, DACL loosened after creation, callback ACE) ran
natively and passed in the released train ed6390f (run 38965604130). Not
exercised: a non-administrator account (unsupported by product rule), the
win_native acceptance machine, and the reparse-point and free-space paths
beyond compilation.
The private state quota is cooperative
admission control for launchers that reserve before writing; same-account code
can bypass it, after which the Controller only refuses further reservations.
Deletion cleanup depends on the Proxy's `deleted_agents` tombstone; an older
Proxy never confirms, and directories of previous server enrollments remain
until an operator removes them. When the enrolled machine root contains the
Treer state directory, file transfer rejects the `agent-private` root by
protected identity, but the rest of the state directory relies on
`machine.file` Policy (follow-up #127).

On the official HTTPS Proxy origins (`proxy.canary.treer.ai` and
`proxy.treer.ai`), the Unix bootstrap verifies the existing stable channel and
version manifest's detached Ed25519 signatures against the embedded official
SPKI. It checks the signed manifest digest and every selected binary's size and
SHA-256 before installation. Node.js 22+ is required before installation-directory
changes. The public bootstrap script itself is trusted through its HTTPS delivery,
not an independently verified script signature. This does not prove freshness:
there is no persistent accepted-version floor, expiry enforcement, or transactional
whole-install rollback. Installing files is not a verified running-process upgrade.
Self-hosted Unix and Windows bootstrap installers retain same-origin SHA256 checks;
Windows also checks matching build identities. Those bootstrap paths do not
independently authenticate the publisher with the official release signature.
Compose image updates do not verify release signatures or enforce an anti-rollback
floor. Signed Controller updates do not yet enforce release freshness or a
persistent highest accepted version. Treer makes no end-to-end supply-chain proof,
SBOM completeness, or reproducible-build guarantee. Actual hosted platform-token
permissions and rotation were not verified by this source review.

Workspace Apps do not uniformly enforce per-principal data ACLs, resource/rate
isolation, or encryption at rest. Secret scanning is heuristic and cannot promise
to detect encoded or unknown secrets. Deploy critical Apps under separate accounts
or externally managed containers with minimal mounts and resource limits; the
shared machine account is not a sandbox.

Treer does not currently provide deny-by-default workspace permissions. Absent
an enforcing Policy, the default permits authenticated workspace actions; monitor
mode does not enforce denials. Use explicit enforce-mode rules and treat workspace
members and enrolled machines as trusted administrators until these are reviewed.

Folders and path Deny rules are not a sandbox or a containment boundary. An
authenticated caller allowed `agent.create` can create other workloads, and a
caller allowed `agent.tree.manage` can reorganize identities into paths with
different permissions. Grant both actions cautiously: a Deny on `/untrusted/**`
alone does not prevent self-moves, explicit creation elsewhere, machine exec,
same-user files or direct networking. Monitor mode does not block any Deny.

New Agent/App creation inherits the authenticated workload caller's current
direct folder unless an authorized explicit destination is requested. Identity,
name and placement are reserved under the workspace transaction lock, not from
environment variables or client-reported paths. Humans/operators default to
root; an unplaced workload caller must request an authorized destination or be
placed by the owner. This removes accidental root placement, not the need for
Policy. Existing nodes are unchanged: legacy unplaced identities do not match
path/relationship selectors, even if the UI displays them at root. Inventory
them and use kind/ID rules or explicitly place them after reviewing the effect
on current rules. Tree move/rename source/destination authorization and secure
deny defaults remain separate migration work; they are not promised here.

Bearer service tokens can be replayed until expiry. Agent tokens and per-request
human forwarding tokens last 60 seconds; human App OAuth tokens last 12 hours.
Membership and service checks are not session revocation: logging out or resetting
a password does not invalidate an already issued App JWT. Workload signature
verification does not uniformly recheck Agent/machine revocation or current Policy.
Do not claim immediate token revocation or single-use bearer tokens.

Policy schema v2 Tree facts are fetched from the current workspace in the
primary database, independently of Provider cache age. A concurrent Tree change
during evaluation rejects the request for retry; a move after acceptance does
not revoke an in-flight operation. Human/machine subjects without Tree placement
do not implicitly belong to root. Links do not grant rights. Defaults remain
monitor/allow; schema v2 does not make an unconfigured workspace deny-by-default.

Owner Tree read/manage and Provider configuration recovery cannot be denied by
delegated Policy; current membership, workspace scoping and structural invariants
still apply. Monitor logs require current owner membership. The original
configured Provider's App diagnostics bridge additionally checks its current
runtime credential, the human token's exact service audience and current service
binding. Its explain/validate/log endpoints retain that owner requirement.
Malicious Provider code can misuse a forwarded Owner token during its lifetime,
so selecting a Provider remains a trust decision. No token or caller attributes
are stored in decision diagnostics. These logs are per-Proxy, bounded and
volatile, and are not tamper-proof or complete durable audit records.

Publication authority defaults to `owner_session`. Only a current owner can
explicitly choose `provider_operator` on the selected Provider binding. This
requires a Core owner session: service-audience human tokens cannot change the
choice, and the App's configuration endpoint is read-only. This prevents a
Provider from persisting authority using an owner's short-lived UI token. It
trusts that App and its management-token holders to edit arbitrary rules,
including their own access. The App checks its private management token; Core
does not receive that token and cannot independently prove that the App checked
it. The separate operator validate/explain endpoints require fresh current
Provider runtime credentials and the database switch on every request; they do
not expose decision logs. Operator explain permits continuous workspace Tree/permission
simulation and enumeration; no dedicated diagnostic rate budget is enforced.
Provider selection and owner recovery remain owner-only. The switch and its
audit are atomic, and changing the bound App or
service resets authority to the default, including old Proxy SQL writes.
Dedicated capability-gated APIs prevent old Proxies from silently ignoring the
choice. This is not bounded delegation or protection against a compromised
Provider. Previously validated in-flight App writes are not instantly cancelled
when an owner switches back. A 0600 management-token file does not isolate
Agents sharing one OS user; use separate OS users/containers and enforced
network/Policy restrictions for that boundary. See the
[publication contract](research/2026-10-13-policy-publish-authority.md).

Schema v2 deployment is not safe with a mixed fleet of old/new Proxy binaries:
old Proxies can convert unsupported bundle errors into configured stale/fail-open
decisions. Upgrade and drain all Proxy replicas before publishing v2; one
replica's capability reply does not certify cluster compatibility. Reverting
Proxy software requires a compatible v1 Provider bundle first. Disable operator
publication and verify App compatibility before rolling back to binaries without
`policy.publish-authority.v1`; 404/missing capability must not trigger legacy
fallback or an unvalidated App write.

App directory capability `app.directory-paths.v1` advertises current canonical
Agent `path` fields derived from the authenticated workspace's Tree. Paths are
included only for principals passing the existing discovery/metadata checks,
and additionally require `agent.tree.read` on the workspace. Denial redacts only
the path, not an otherwise visible principal. Paths grant no permissions. Humans,
unplaced legacy workloads and principals without Tree read have `path: null`.
Apps needing placement must explicitly report unavailable on older
Proxies without this capability, rather than infer paths from names or IDs.

Service-audience directory and recipient resolution apply Core Agent discovery/
metadata and human-list rules too; a denied Agent cannot be discovered through
an App directory instead. The currently verified human's own identity remains
available without enumerating others, allowing Provider authentication even
when delegated human-list access is denied. Monitor still exposes authenticated
workspace metadata; use enforce with explicit defaults to hide resources.

PostgreSQL and NATS are trusted control-plane backends, not tenant isolation
boundaries. Opaque login/admin/ingress sessions and signing-key metadata require
confidential database access and backups. The Compose NATS service has no broker
authentication; its published ports are loopback, but its private Docker network
must also be trusted. Use network isolation, least-privilege backend credentials
and transport protection for external backends. Treer does not guarantee global
login throttling, workload quotas, bounded WebSocket backlogs, complete audit
delivery, or protection against backend administrators modifying audit records.

Workspace audit (`workspace.audit.v1`) is a sensitive read, not an ordinary
monitor/allow-default operation. Only current workspace owners (including
organization managers) have baseline access. Other members, Agents and machine
identities require an explicit matching `workspace.audit.read` allow rule in
enforce mode; action-default allow and monitor rules cannot grant access. Enforced
deny can restrict an owner too; organization audit access remains unchanged.
Service-audience App tokens are authorized as their represented caller and bound
to the token audience's workspace, never as a privileged App deputy. A workspace
owner grant does not grant access to another restricted workspace in the org.

Workspace audit responses use exact workspace scoping and no-store, discard
stored actor/resource display names and correlation IDs, and project payloads
through per-action scalar allowlists. Paths, hashes, commands, argv, environment,
configuration, prompt/message bodies and unknown payload keys are not returned
by this endpoint. Stable actor/resource IDs and event types remain metadata; this
does not make the underlying database or existing organization audit view secret
free. No unattributed history is reconstructed. Reading audit is not itself
audited. The audit summary (`workspace.audit-summary.v1`) is the same sensitive
read with the same authorization, scoping and no-store rules; it exposes only
counts over the feed's rows, exact actor and resource IDs the feed already
shows, current agent/machine/App display names trimmed to 128 characters, and
action names. Chart categories are presentation, not authorization: the
server-side table maps action names to eight categories, unknown actions fall
into `other` and are logged, and the mapping never widens what a caller may
read. Summaries are rate- and concurrency-limited per Proxy and fail closed
(503) rather than return partial counts. The typed-instant backfill run by the
maintenance worker writes only that derived column of existing rows, newest
first, and marks unparsable instants instead of guessing them. Tree and enrollment audit writes share their mutation transaction;
networking and accepted prompt audit writes are best-effort after success.
The latter contain only authenticated actor/target IDs and event kinds, with no
body or configuration. New events retain immutable workspace attribution after
workspace deletion; already-unattributed history is not reconstructed, and
organization deletion still removes its events. Runtime audit delivery can fail
after a successful mutation, and database/Host operators can modify stored events:
this is neither complete, tamperproof nor non-repudiable auditing.

Prompt audit admission is a bounded durable PostgreSQL outbox, not a guarantee
that every accepted runtime mutation is audited. Queue overflow increments a
durable global counter and warns; unconfirmed admission warns with a separate
process-local counter without database error values. No prompt bodies enter the
outbox. Worker insertion/removal is atomic and deduplicated by event ID. Workspace
metadata/statistics use the same sensitive-read authorization and exact tenant
scope as their audit feed; global queue counters are operator/database metadata.
All organization audit kinds, including authentication, membership and policy,
expire under the same configurable default 180-day policy. Retention
configuration has a 30-day minimum; changes emit a platform event and numeric
startup log. Capacity is shared without per-tenant fairness: a tenant can cause
counted prompt-audit drops in another tenant, while accepted prompts themselves
and transactional security-kind writes are unaffected. Scoped deletion
watermarks describe removed history, not completeness. Database/Host operators
can change retention and stored events; retention is not an immutable compliance
archive, and does not expire independent backups, platform audit or Policy logs.

Proxy help hides configured administrator-password and NATS-URL environment
values, alongside existing secret arguments. This is diagnostic hygiene, not a
general guarantee of log redaction: arbitrary command/environment values and
debug HTTP URLs may still contain user-provided secrets. Avoid dumping process
environments, configuration files, request queries, or startup snapshots in support
logs; protect log and backup access with the same account boundary as live secrets.

Unknown embedded-launcher installer errors expose at most 300 characters of
single-line prose. The Controller removes the enrolled root, configured credential
environment values, control/format characters and path/token-shaped words before
printing that diagnostic; raw installer stderr is not forwarded. This does not
make arbitrary third-party error strings safe to contain secrets.

Do not describe Treer as zero trust, mutually untrusted multi-tenancy,
end-to-end encrypted from the Proxy, per-user provider credential isolation, or
a filesystem sandbox. The current coding-agent launch modes can execute with
the machine account's authority. Same-account processes may inspect files,
process metadata, local configuration, or credentials available to each other.
Machine exec and file upload are likewise not restricted administration APIs:
they run or write with the Controller account's filesystem permissions. Host
root containment prevents accidental path targeting outside the enrolled tree,
not access that the same account can obtain through commands, symlinks, or other
same-account processes. Eight active 128 MiB uploads can stage up to 1 GiB in
the destination directories of one machine for up to six hours; the free-space
reserve (declared size plus the larger of 5 percent and 128 MiB, plus the other
active declared sizes not yet written) keeps the volume from filling, not from
being busy.

Agent startup specs are an explicit persistence capability, not a sandbox or a
reliable operating-system boot service. The machine-local private state file
contains the startup argv and plaintext workload credential. Do not place
secrets directly in argv; use files or a secret provider already protected for
the machine account. Anyone with that account's authority can modify the same
state or execute an equivalent command.

Browser sandbox flags do not isolate trusted App or Agent Interface JavaScript
from the user's Proxy authority. Embedded pages served through the Proxy-origin
tunnel share the Proxy origin and can
make authenticated requests to that origin; if the control plane is also hosted
there, `allow-scripts` plus `allow-same-origin` permits access to the parent page.
Agent interfaces can instead be embedded on their own `ag-<id>` origin through
an embed session (#156, below), which removes the shared origin; the tunnel
remains the fallback for browsers that block the partitioned cookie unless
the operator sets `--agent-embed-fallback refuse`.
Embedded interfaces may also open a new browser tab (`allow-popups` and
`allow-popups-to-escape-sandbox`), for example to show the same interface full
page. These popups are unsandboxed top-level pages; same-origin destinations
retain that origin's session. Unless the interface uses `noopener`, the popup's
opener is the embedded frame, not the Console. The iframe still has no
top-navigation permission. Interface authors should use `noopener,noreferrer`
for external opens. These flags do not make the embedded App a trust boundary.
The Console HTML document is served with `Cross-Origin-Opener-Policy:
same-origin` (Proxy `app_dist`, the Worker and the nginx image alike, #182):
a popup of a different origin opened from the Console or from one of its
frames gets its own browsing context group, so its `window.opener` cannot
reach the Console; same-origin popups can keep a compatible opener. COOP
protects only against opener-based access from cross-origin popups; it does
not isolate iframes, network requests or embedding by other sites, and it is
set on HTML responses only, never on APIs, tunnels, callbacks or assets. The
iframe sandbox and the Console's own `noopener` practice are unchanged.
Native WebView bridges currently lack a complete frame-origin and navigation
boundary. HttpOnly cookies prevent direct script reads, not authenticated API
calls or bridge abuse. Do not open untrusted interfaces with a privileged user
session. See the [2026-10-12 Web security review](research/2026-10-12-security-web.md)
for local reproductions, remaining work, and verification limits. CSP and
anti-framing headers are defense in depth, not evidence of a complete XSS,
CSRF, or mobile WebView security audit.

Apps do not create a security boundary. Managed Apps currently run through the
same Host process runtime as command Agents, under the installing machine
account. Externally managed Apps inherit their supervisor's authority.
Both managed kinds share a stable workload identity and lifecycle. Stop keeps
that identity and its credential while disabling recovery; deletion revokes it.
Stop is idempotent: a workload whose launch failed before the Host spawned a
process (for example an App created with a working directory the Host refuses)
is reported as exited from its saved declaration, a Host answer that the target
process is absent completes the stop, and deletion then proceeds, so a failed
launch never leaves an undeletable record (#150). App creation and update
refuse absolute working directories up front; the Host's own check below the
machine root remains authoritative at launch.
Start requires `agent.create` on the owning machine, stop requires `agent.stop`
on the workload, and restart requires both. Legacy App routes use these same
checks. Reconnect adopts active legacy identities without rotating live secrets;
only a missing local credential permits rotation of an active identity. An old
App identity already revoked by the previous stop implementation migrates to
the App's canonical identity rather than reviving the revoked credential.
Treer Policy limits what authenticated requests Core accepts; it cannot stop an
App from using other same-machine credentials or services it can reach. Run
untrusted code under a separate user, container, VM, microVM, or stronger
sandbox.

Agent Interface Servers likewise do not create a security boundary. They run
with the Agent's operating-system authority and may expose transcript and prompt
operations to callers already authorized by Proxy Policy. Interface ports use
machine loopback, which is shared without external isolation; the Controller is the external routing and policy
boundary.

For registered Agent Interface UI tunnels, Core strips client `x-treer-*` headers
and injects `X-Treer-Interface-Source: proxy-ui` with the resolved Agent and current
interface instance. Controller semantic AIS requests use `controller-api` with
the same target binding. Strict launchers check these markers, loopback Host and
browser Origin/Fetch Metadata to reject direct cross-origin HTTP/WebSocket and
DNS-rebinding requests. The values are public, not secrets: local non-browser
clients can forge them and remain within the trusted machine-account model.
UI provenance is not sender attestation or per-command Policy authorization;
only Controller semantic calls may attest queue senders. Old Core lacks provenance
and must be updated before strict launchers are enabled. See the
[forwarding contract](architecture.md#browser-defense-forwarding-provenance).
The launcher checks the actual listener port on every HTTP request and WebSocket
upgrade. Do not claim mutually untrusted local-process isolation. Export's
no-Origin heuristic is not independent archive authentication. See the
[local launcher audit](research/2026-10-12-security-launcher.md).

Release Controllers embed a same-commit launcher manifest and payload. The local
`launcher install/update/rollback` path verifies embedded/extracted bytes before
executing the installer, checks the actual Controller contract, and refuses
retained versions below its minimum launcher security epoch. Release builds
require a prepared portable runtime; debug source installs need explicit network
opt-in. This authenticates assets through the Controller release, not the local
Node publisher: operators must trust the selected Node/CLI and their paths.
Unix executable ancestors are checked for trusted ownership/write permissions;
Windows executable ACL auditing is not implemented. Private state uses the
existing current-user ACL/0700 boundary, which does not isolate same-user code.
Subprocess output is bounded, stderr is discarded, and Node environment hooks
are removed; trusted CLI children still need the caller's authenticated identity.
Hashes do not prevent an equal-privilege account from replacing installed code
after verification. No launcher installation restarts an existing workload.

Native Codex/Claude and ACP launch modes default to unattended full authority
and inherit the Agent's own Treer credential for tools. Session export excludes
known credential/configuration files, but it does not redact secrets embedded
in allowed conversation or memory files. Transcript, provider stderr, tool
output and completion notifications likewise have no general content-redaction
guarantee. Treat exported archives and histories as sensitive data. Message
source attribution and non-system-instruction warnings do not guarantee that
the model will resist prompt injection or decline malicious instructions.

Repository launchers are optional code executed with the Agent's operating-
system authority. Treer's binary installer, updater, and release artifacts do
not fetch or install them. The optional `launchers/agent` launcher is obtained
separately; its explicitly invoked installer owns harness dependencies and UI
setup. Ordinary Agent startup does not fetch or update software. Its browser
UI is served through the private Agent Interface tunnel. Selecting a launcher
trusts that launcher and its selected harness with the Agent's authority; a
private UI tunnel does not create an operating-system sandbox.

Treer does not install software by default, capture network traffic, manage OS
DNS, create namespaces, or configure a sandbox. The one exception is the
explicitly confirmed, release-pinned
[provider CLI installation](#provider-cli-installation). Profiles and external
launchers own the remaining choices. Ordinary internet traffic is outside Treer's authorization and
metering boundary.

The thin CLI carries an Agent credential separately from the Controller's
operator and machine credentials. Agent mode requires both Agent ID and its
credential; missing credentials must not fall back to an operator identity.
This enables a caller-provided isolation boundary to expose only the Agent's
identity. It does not protect credentials from another process under the same
unisolated account, or from the machine owner.

Each managed Agent/App HTTP proxy entry attributes requests to its fixed runtime
identity, independent of PID or caller-supplied headers. The proxy uses existing
Agent credentials and service Policy; it strips local Treer credentials and
untrusted identity headers, then issues audience-bound HTTP tokens for registered
service addresses only. Service names resolve inside the authenticated workspace.
HTTP pins the stream to the canonical token audience; CONNECT resolves its
selector before checking Policy against the stable ID. Renaming changes a
convenience address, not the identity or permission key. HTTP CONNECT carries opaque TLS/native protocol bytes,
without injected target credentials. Ordinary external traffic is forwarded
without Treer identity and is outside Core authorization/metering. A local
process able to reach another workload's entry can use that identity. Entry ports
are not secrets, and Treer does not prevent bypassing the proxy. Use external
network/account isolation when reachability itself must separate identities.

Service TCP connections are reauthorized periodically and closed when their
identity, service, membership, mapping, or Policy no longer permits them.
Polling and Policy caching mean revocation is not instantaneous. New anonymous
public TCP ports require an explicit public action; the application owns its
protocol authentication. There is no claim that arbitrary outbound networking
is restricted or audited.

## Credentials

Platform administration exposes aggregate statistics, not tenant inventories.
Removed `/api/admin/users`, `/machines`, `/agents`, `/organizations`, `/activity`
and their former nested/mutation routes return a uniform `410 admin_details_removed`
after administrator authentication, without testing whether an ID exists.
`GET /api/admin/traffic` requires the existing admin cookie (ordinary human,
operator, workload and App tokens do not substitute for it); the explicit
loopback-only `--disable-auth` development mode retains its documented bypass.
It aggregates persisted hourly usage across workspaces by traffic class, omitting
all identity and routing fields. Statistical aggregation is not differential
privacy: sparse traffic/counts and comparison across time may permit inference.
This is an HTTP/API data-minimization boundary, not protection from database or
host operators. Internal audit records and their existing retention remain;
the platform activity feed is no longer exposed through the admin API.
Admin aggregate, invitation and retired-detail responses set `Cache-Control:
no-store`. Registration invitation lists expose only the latest 100 non-bearer
handles, at most six safe prefix characters, creation time and used time;
short/nonstandard legacy tokens show `[redacted]`. Handles are domain-separated
SHA-256 hashes of the bearer, not registration credentials, and DELETE requires
the administrator cookie plus the handle. Full tokens/URLs are returned only
in the successful create response; new invitation audit events store the handle,
not the bearer. Existing stored audit records are not rewritten by this change.

| Credential | Scope and limit |
| --- | --- |
| Enrollment key | One workspace, ten minutes, single use |
| Machine bearer credential | One enrolled machine and workspace; long-lived until rotation/removal |
| Agent workload credential | One stable managed workload across process generations; same-account inspection remains possible |
| Local operator credential | One Controller install; protects the local API but is not a same-account sandbox |
| Workload identity token | One Agent/machine/service audience for 60 seconds |
| Human App token | One user/workspace/service audience; 12 hours via OAuth, 60 seconds via authenticated browser forwarding; verification rechecks membership and service |
| Human user session | Cookie `treer_session` for browsers; legacy native clients can still receive the same token in JSON when `X-Treer-Client` is exactly `mobile`, `mobile_ios`, or `mobile_android`. `Authorization: Bearer` is accepted on every user route. The header is not CORS-allowed. Do not put the token in query strings |
| Platform admin session | Cookie scoped to `/api/admin`; separate from user accounts; aggregate counts/traffic, platform invitations and image updates; no user/machine/Agent/organization inventories or activity detail |
| Updater token | Shared Bearer secret between Proxy and the Compose updater sidecar; never exposed to browsers |
| Mail cookie | Local opaque handle to an App token; compromise of Mail state grants that token until expiry |
| Telegram bot token | One Telegram bot; Telegram and any process that can inspect it can act as the bot |
| Release signing key | All official release manifests; must remain offline and outside runtime systems |

Telegram numeric user/chat/topic allowlists are channel admission, not Treer
authentication. Inbound Telegram Messages are authored by the authenticated
bridge Agent and retain sender-asserted external metadata.

## Data Exposure

Workspace folders and placements are presentation metadata, available to
current workspace members and authenticated workspace-bound Agents/operators.
Agent/operator tree reads require `agent.tree.read`; mutations require
`agent.tree.manage`, both on the workspace's `agent.tree` resource.

The flat workspace inventory is membership-based (#141): `GET
/api/workspaces/{ws}/snapshot` and the browser event stream
(`/api/workspaces/{ws}/events`) give every current member the same facts about
every workload and machine: IDs, names, kinds, status, machine IDs, the
workload's working directory (`cwd`, a path on the user's machine), build
versions, the machine root and the machine's allow-listed upload capabilities
(`machine.upload`, `machine.upload.v3`; the Controller's full capability set
stays in the Proxy connection table, #173). The workload `pid` is not part of
the browser-visible inventory (#153): a request authenticated with the session
cookie receives an allow-listed Agent shape without it from the snapshot, the
event stream (including replayed or buffered events, which are re-serialized
through the view at delivery) and the Agent routes, while a `Bearer`
credential (a human's CLI or API token, an Agent token, a machine token) keeps
the full `AgentInfo`, still subject to membership and Policy `agent.discover`.
The split is by credential kind, not by client name, so a member who holds a
CLI token can still read `pid`; the change removes it from browser-delivered
data only. The view is chosen with the same `Bearer` parser that
authentication uses, so a cookie session cannot obtain the trusted shape by
adding a malformed `Authorization` header. App deployments follow the same
rule: their runtime `pid` is omitted from browser responses of the `/apps`
routes and kept for bearer, Agent and machine callers. `cwd` remains visible to members and is the next candidate for
minimization once the Console has an explicit affordance for it. Policy
`agent.discover` filters this list
for machine and Agent principals only; applying it to browser sessions is a
separate decision (#152), as is minimising `cwd`/`pid` (#153). The stream
never carries folders, placements, paths or pin names: `workspace.snapshot`,
`server.*` and `agent.*` events hold the flat facts above (a test pins their
field sets) and `workspace.pins.changed` holds IDs; the Tree and pins are read
through `GET /workload-tree` and `GET /pins`, which authorise per request with
`agent.tree.read`. Membership and the session are authenticated when the
stream connects and re-checked afterwards with the same lookups the request
middleware uses: before a delivery once more than 10 seconds passed since the
last check, and on a 30 second tick while idle. A session that was logged out,
expired or belongs to a deleted or disabled user closes the stream with
WebSocket code 4401; a user who is no longer a current member (organization
role removed, group membership or user grant removed) or whose workspace was
deleted gets 4403. The close frame carries the code only. Revocation therefore
takes effect no later than the first event after 10 seconds or the 30 second
idle tick, whichever comes first; a database failure during a re-check keeps
the subscriber until the next check and grants nothing new, and if the session
or membership cannot be re-verified for 120 seconds the stream closes with
code 4503. These
permissions grant shared workspace organization, including peer placement,
without process-control authority. Browser membership authorization is
unchanged. Folder moves cannot change a workload's machine,
credential, filesystem path, or Policy scope. They do not create an isolation
boundary. The Proxy validates workspace ownership and rejects folder cycles.

Proxy and database operators can read Message bodies, recipients, context
edges, and acknowledgement state. Deployments must manage PostgreSQL backups,
retention, export, and deletion because Core does not yet expose those operator
workflows.

Mail can read any body it renders or sends and stores pending PKCE state plus a
cookie-to-token mapping in SQLite. Telegram can read bridged bodies and stores
Bot API offsets, delivery hashes, errors, and external/Core ID mappings. App
state and WAL files require normal credential-store protection and backup.

Public service ingress deliberately accepts anonymous internet traffic, so the
Proxy protects it generically (#146) before anything reaches a machine tunnel:
every public request is admitted first against a per-client token bucket
(default 120 requests/min, burst 30, keyed by the verified client address, IPv6
by /64) and only then against the service's own bucket (600/min, burst 120)
and in-flight cap (64, held until the response body or upgraded connection
ends); refusals are `429` with `Retry-After`. A noisy client therefore exhausts
its own budget first, while a distributed flood from many addresses can still
drain the service bucket — that is the intended trade for protecting the
machine. The resolver answers unknown hostnames from its in-memory snapshot
(refreshed every few seconds) so floods of random hosts do not become database
queries, and those 404s are themselves limited per verified client (never per
front address, and not at all for a private peer without a configured front,
so an unconfigured load balancer can never throttle a whole site). Request
bodies are capped (1 MiB; `Content-Length` above the cap is refused before a
tunnel opens, a streamed body is cut at the cap), must keep flowing (10 s idle)
and must complete within 60 s in total (owner-adjustable), so a trickled body
cannot hold an in-flight slot; the service must answer headers within 35 s
(long polls up to 30 s work); a non-upgraded response body may not stall for
300 s and its in-flight slot is leased for at most 300 s even if the client
never reads the response. Streams longer than 300 s therefore continue without
counting against the in-flight cap; they remain bounded by the connection
budget and the per-client rate. WebSocket upgrades hold their slot for the
connection's lifetime. The HTTP/1 server
itself enforces a 30 s header read timeout with 32 KiB / 100 headers per
request and caps open connections (8192 in total; 256 per peer for public,
untrusted peers only — a configured trusted front or a private/loopback peer
carries many clients on one address and is bounded by the global budget alone;
excess connections are closed without a handshake). It speaks HTTP/1 only, as the
Proxy did before (the HTTP/2 feature was never enabled, so no h2c was ever
negotiated); TLS fronts talk HTTP/1 upstream. Requests carrying both
`Content-Length` and `Transfer-Encoding` are refused. Ordinary API routes keep
their existing extractor body limits; the body read timeouts described here
apply to ingress only. Workspace owners may loosen or tighten a Managed App's limits
through `PATCH /api/workspaces/{ws}/apps/{app}/ingress-limits`, audited as
`ingress.limits.update` with the numbers only, within hard ceilings
(60 000/min, 4096 in flight, 64 MiB, 5–600 s for both the response and the
body deadline); nothing can switch protection off. The per-client bucket table
is bounded per ingress and swept at most every ten seconds, so a flood of fresh
addresses shares one overflow bucket instead of forcing a scan per request. All of this is per Proxy instance and in memory: a cluster of N multiplies
the ceilings by N, and rejection counters (reported per ingress, counts only,
never addresses or paths) reset when the process restarts.

The client address the Proxy enforces and forwards is the TCP peer, unless the
operator names a trusted front (`--ingress-trusted-proxies` CIDRs plus exactly
one `--ingress-client-ip-header`); only a peer inside those ranges may supply
the header, `X-Forwarded-For` is read right-to-left skipping trusted hops, any
other header must hold exactly one address, and a missing or invalid value
falls back to the peer (counted as `client_ip_fallback`, which usually means
the front is misconfigured). `/0` ranges are never accepted and ranges wider
than /8 (IPv4) or /32 (IPv6) need `--ingress-allow-wide-trust`. The Proxy
forwards the verified address as `X-Treer-Client-IP` after stripping every
client-supplied `x-treer-*` header (upgrade requests included); Apps must use
that header and nothing else for per-client accounting. Behind a front that is
not configured, every client would share the front's address, so when the peer
is a private, loopback or link-local address and no trusted range is set, the
per-client limit is not enforced (the service limit still is) and the Proxy
logs a fixed warning once; per-client limits are effective only with a public
peer or a configured front, which #142 must state. Private, loopback and
link-local peers are therefore always "front or internal" to the Proxy: a
Proxy exposed directly to untrusted clients on a private network relies on the
per-service limits and the global connection budget alone.

Workspace ingress gets the time bounds (body idle, response header, response
idle) and the framing check but no rate limits and no default body cap:
authenticated Apps may legitimately upload larger files. Public TCP services
are outside this protection.
Workspace ingress requires a current member session or service-audience token.
For restricted workspaces, current membership means an effective direct or
group workspace grant, workspace creation ownership, or organization manager
access. Authorization is recalculated from PostgreSQL, and removing a user from
the organization invalidates all workspace access.
The Proxy strips gateway credentials and Treer headers before forwarding, but
it remains in the browser-to-service data path. The Proxy records directional
payload byte and frame totals for these tunnels under the synthetic `browser`
client endpoint. Each aggregate carries a traffic class and meter version so
future billing rules remain explainable; workspace members can read those
aggregates. It does not store payload content, request paths, headers, or NATS
control-plane traffic in the usage ledger.

The authenticated Managed App and Agent interface UI tunnels require current
workspace access and resolve the target within that workspace before forwarding.
They replace only CSP `frame-ancestors` with the configured control-plane origin
and `'self'`, and remove conflicting `X-Frame-Options`, so the control plane can
embed the human interface. Other CSP directives, Permissions-Policy and
Referrer-Policy remain
enforced; direct App origins and ordinary virtual-host/service tunnels retain
their embedding policies. Top-level Agent UI navigation is unaffected. This
limits same-site sibling origins from framing the authenticated Agent UI, but
does not isolate Agent JavaScript from the Proxy authority described above.
Embedded content remains trusted workspace code, not an isolation boundary.

Known Proxy transport failures on a Managed App root iframe GET may return a
nonce-restricted, noncached HTML error bridge with the original failure status.
Its `treer.app-ui.unavailable` message targets only the configured Console
origin and contains the workspace and App IDs, never transport diagnostics or
credentials. The Console must validate the Proxy origin, current iframe source
and both IDs before bounded recovery. This is a navigation failure hint, not a
new authentication capability or proof of application health. Normal upstream
responses, API/HEAD calls and authentication/CSRF failures are not rewritten;
the bridge does not replay App writes. Trusted App code sharing the Proxy origin
can imitate such a hint, so it does not establish a stronger trust boundary.

### Agent interface origins

When wildcard ingress is configured, an Agent's registered interface UI is also
reachable standalone on its own origin, `https://ag-<32 hex>.<apps domain>/`
(#143 Phase 2). The origin is a workspace-access row in `service_ingresses`
with `target_kind = agent_interface`; it cannot be switched to public, has no
owner-editable limits, is created only by the authorize step below, and is
deleted with the Agent together with every ingress session on it. An ingress
session has its own 12-hour lifetime, independent of the Console session's
expiry: membership and Policy are re-checked on every request, and Console
logout ends the user's ingress sessions on every workspace host early.

The origin accepts only a workspace ingress session cookie: `Treer-Authorization`
Bearer tokens are refused (`401 ingress_authentication_required`), so an Agent
or service token never reaches another Agent's UI this way. Before any session
exists, every well-formed `ag-` hostname behaves identically (a top-level
navigation redirects to the Proxy authorize endpoint, a subresource is refused,
Bearer is refused) whether or not the Agent exists; existence is decided only
after the Console session is authenticated. Authorize then requires current
workspace membership and the interface gate, `agent.output.read` **and**
`agent.prompt` on that Agent for the human subject, before it creates the
origin row and issues the single-use, browser-bound code. The gate runs again on
every request to the origin, so a later Policy denial or membership revocation
refuses the existing session; the same gate now protects the Proxy-origin
`/agents/{id}/interface/ui` tunnel for humans and machines. Hydrating
`interface.origin_url` on the Agent and snapshot read models uses the same gate
and never creates the row; its result is remembered per (workspace, member,
Agent, Policy revision) for five seconds on each Proxy instance (a bounded
cache, cleared when it exceeds ten thousand entries), so an interface
re-registering in a loop cannot turn every member's Console refetch into
unbounded Policy evaluation; the Controller additionally refuses more than ten
interface registrations per minute per authenticated Agent
(`agent_interface_rate_limited`, counted after authentication, the existing
registration left intact, a bounded map of tracked Agents). The revision is the one the Policy evaluators
would apply now, so a published Policy is re-evaluated as soon as the engine
sees it; one member's result is never served to another; allowed and denied
results share the window, a failed evaluation is never remembered, and when
the Policy document cannot be loaded the cache is bypassed entirely. The
authorize step, every request on the origin and the Proxy-origin tunnel
evaluate the gate uncached.

Requests are tunnelled with the Agent UI provenance (`proxy-ui`, the resolved
Agent ID, the registered instance; cookies and gateway authorization stripped,
client `x-treer-*` removed) to the registered loopback endpoint under the
registered `ui_path`. Responses keep the Agent's CSP untouched (no
`frame-ancestors` rewrite: the standalone origin is not embedded) and receive
`X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer` when the
Agent did not set them.

#### Embed sessions

The standalone authorize flow cannot run inside the Console iframe: the
authorize step requires a top-level navigation carrying the Console cookie,
and the Agent origin's cookies are third-party in a frame. Embed sessions
(#156, capability `agent.interface-embed.v1`) therefore split the flow. The
Console, with its own session and the ordinary Console-origin CSRF rule,
`POST`s `/api/workspaces/{ws}/agents/{id}/interface/embed-authorization` with
the hash of a nonce the frame generated; the Proxy applies the same gate as the
standalone authorize (membership, with a non-member answered as an unknown
Agent; `agent.output.read` and `agent.prompt` for the human; interface
registered) and answers a single-use code that lives 30 seconds and is bound
to the user, the Agent, its `ag-` ingress row and that nonce hash; the response
is `no-store` and carries no CORS allowance. The iframe loads the Proxy-owned
bootstrap page `https://ag-<id>.<apps domain>/.treer/embed` (strict CSP with a
per-response script nonce, `connect-src 'self'`, `frame-ancestors` equal to the
Console origin, `Referrer-Policy: no-referrer`, `no-store`; it reveals nothing
about the Agent). The page announces `{treer_embed: "ready", nonce_hash}` to the
Console origin only; the Console must accept it only when `event.source` is the
iframe's window and `event.origin` is the expected `ag-` origin, and answers
`{treer_embed: "code", code}` with that exact `targetOrigin`; the page accepts
the code only from the configured Console origin and never uses `*`. The page
redeems code and nonce on its own origin (`POST /.treer/embed/session`, subject
to the Fetch Metadata rule below); the Proxy consumes the code before checking
the nonce, re-checks membership, and sets
`__Host-treer_ingress_embed=…; Path=/; Secure; HttpOnly; SameSite=None;
Partitioned; Max-Age=43200`. The page then calls `GET /.treer/embed/check`
with credentials: `204` proves the partitioned cookie is sent back and the
frame navigates to the Agent UI; anything else (older Safari and other browsers
that block third-party cookies, a timeout, a failed redeem) posts
`{treer_embed: "cookies_blocked", nonce_hash}`; the hash is present once the
page computed it, and the Console accepts a failure report after `ready` only
when the hash matches the one it accepted: the Agent document that later
replaces the bootstrap shares the frame's window and origin but never learned
the hash, so it cannot imitate a bootstrap failure to pull the Console back to
the shared origin (a hash-less failure is accepted only before any `ready`).
On such a failure the Console never switches origins by itself: it shows a
notice and lets the user choose between the shared-origin Proxy tunnel,
labelled as such, and opening the Agent in a new tab; where the operator set
`--agent-embed-fallback refuse` only the new tab is offered (the tunnel then
answers `503 embedded_ui_requires_isolated_origin`). Mint errors and
timeouts lead to the same notice, never to an automatic downgrade. Codes are never logged.
Session reuse (#198): a Console that loads the bootstrap with `?reuse=1` (a
capability hint that changes no authorization decision) receives a version-2
`ready` with the nonce hash, answers with a probe that carries only type,
version and the echoed hash, and the page then calls the check on its own
origin; a `204` means a live embed session for this origin is already in the
frame's partition, so the page loads the UI and posts `reused` with the same
hash and no code is minted; any other answer makes it post `mint` and the
handoff runs as before. The page honours one probe, only from its parent at
the configured Console origin and with its own hash; the Console runs a
one-way state machine bound to the first hash and never mints twice. The check
answers `204` only under the full request-path gate: session live for this
host and kind, current membership, and the interface Policy pair for this
Agent evaluated without recording a decision; every other state is the same
`401` as "no session" with no reason, so the check reveals nothing the UI
request would not, and no party caches a positive answer. The check honours
the embed cookie only under the same request-context rule as the UI request
(exactly one `Sec-Fetch-Site: same-origin`), so a foreign context holding the
cookie gets the same `401` and the check is no authorization oracle. Reuse changes no
binding, expiry or revocation: the same cookie is presented, logout and
membership removal end it, and the UI request and the socket re-check apply
the gate again. Handshake and UI responses carry no `Server-Timing`; load-time
measurement is a development-only, fixture-only runner on the Console side.
Prewarm (#198 step 3, capability `agent.interface-embed-prewarm.v1`): a Console
that advertises intent (pointer dwell on a Tree row) may mint with
`purpose: "prewarm"`; such a code redeems into a 10-minute embed session (a
client that lies about the purpose can only shorten its own session), prewarm
mints have their own authoritative budget of 6 per user per minute across all
tabs and devices on top of the 30 per minute and 8 outstanding codes, and every
mint is audited (best effort, after the mint) as `agent.interface.embed.minted`
or `agent.interface.embed.prewarmed`, both in the `apps_ingress` category so
they never inflate lifecycle counts and a hover is never read as an access. The
hidden prewarm frame loads the bootstrap with `reuse=1&prewarm=1`, rendered
server side only for that exact pair; its only difference is that it never
navigates to Agent HTML: a verified session ends in the terminal
`{treer_embed: "warmed", version: 2, nonce_hash}` and any failure in the quiet
`{treer_embed: "warm_failed", version: 2, nonce_hash}`. The click then runs the
normal reuse handshake; the first authorized document navigation of a prewarm
session rotates it into a normal 12-hour session: the mark and the new row are
written in one transaction, only after the upstream response that carries the
new cookie exists (a tunnel failure or a refused request leaves the prewarm
row unrotated, so the next navigation can still rotate), the old row is kept
until its own 10-minute expiry and never extended, and subresources, fetches
and WebSocket upgrades never rotate. Live embed sessions are capped at 8 per
user and Agent origin under a per-pair lock: rotated rows are outside the cap
(they die by their own expiry, bounded by the prewarm budget), eviction takes
unrotated prewarm rows first and then, for a normal insert only, the oldest
normal row; a prewarm never evicts a normal session and is refused instead,
and the row just issued is never evicted. Logout, membership removal and
Policy deny end prewarm sessions exactly like any other.
Agent UI. Embed
sessions are separate rows from standalone sessions: the token of one never
authenticates through the other cookie. The request context is decided before
any credential is read: for an ordinary request the embed cookie grants authority
only with exactly one `Sec-Fetch-Site: same-origin`; for an upgrade it
additionally requires exactly one `Origin` header byte-equal to this Agent
origin in every case, and `Sec-Fetch-Site` may be absent only on a real WebSocket
upgrade (`Upgrade: websocket` with `Connection: upgrade`), because Chromium sends
no Fetch Metadata on an iframe's same-origin handshake. In every other context
(`none`, `same-site`, `cross-site`, a missing header on an ordinary request,
duplicated headers, any other or missing `Origin` on an upgrade, any other
`Upgrade` value) the embed cookie is treated as absent
and the request takes the standalone path: the standalone cookie if the browser
sent one, else the authorize redirect for a navigation or
`401 invalid_ingress_authorization`. An unusable embed cookie therefore never
turns into a `403` for a request a standalone cookie would authorize, which
matters because a browser whose Console and apps domain share one site (the
partition key is the site, not the origin), or that ignores `Partitioned`,
sends the embed cookie on standalone visits too. The same rule stops a page on
one Agent origin, framed by the Console, from spending the embed cookie on a
sibling Agent origin: that request is same-site, so the cookie is ignored.
A tunnelled WebSocket, standalone or embedded, is authenticated at the upgrade
and then re-checked by the Proxy every 30 seconds (plus up to 5 seconds of
per-socket jitter) with the same lookups as the HTTP path: the ingress or
embed session by hostname, token and kind, current workspace membership, and
the Agent interface gate evaluated without recording a Policy decision. A gone,
expired or foreign session closes the socket with code 4401, lost membership or
a Policy denial with 4403, so Console logout, membership removal, the 12-hour
expiry and a Policy change end an open socket within about 36 seconds. The same
re-check covers Console-session sockets on the Proxy-origin interface tunnel
and ingress-session sockets on workspace App hosts; public and Bearer upgrades
are not re-checked. The checks run beside the relay, each bounded to 5 seconds,
so a slow lookup never pauses the socket. Database errors, Policy-store errors
and timed-out checks keep the socket (nothing new is granted) and are retried;
after three consecutive failures, about 120 seconds worst case, the socket
closes with 1011. Any other lookup answer than a matching live session ends the
socket at once with 4401. The close code is best effort because the
relay is byte-level; closure within that window, including the Agent-side
half, is the guarantee. Responses to
requests carrying the embed cookie get `frame-ancestors` replaced with the
Console origin and `X-Frame-Options` removed; standalone sessions keep the
Agent's CSP untouched. Membership is re-checked on every request and Console
logout and membership removal end embed sessions like standalone ones. The
Agent's own JavaScript running in the frame can post `ready` again and have
the Console mint and relay further codes, and can redeem them: the embed
session is therefore not a secret from the Agent page (the same trust as the
Agent's own UI). The isolation gained is separation from the Proxy origin and
the Console, not from the Agent; the Proxy bounds minting to 30 codes per user
per minute and 8 outstanding unused codes per user (`429 embed_rate_limited`),
and the Console answers at most one `ready` per frame navigation.

A `Partitioned` cookie is keyed by the top-level **site** (scheme plus
registrable domain), not the origin. When the Console and the apps domain
share a registrable domain, a top-level visit to an `ag-` host and the Console
embed share one partition, so partitioning alone separates nothing; the
barriers are the host-only `__Host-` cookie, `frame-ancestors` limited to the
exact Console origin on embed responses, and the Fetch Metadata rule below,
which refuses same-site and cross-site browser requests to the Agent origin.
Operators should give the Console and the apps domain separate registrable
domains where possible. Browser support for `Partitioned` (Chrome/Edge 114+,
Firefox 141+, Safari 26.2) is verified in the canary window; browsers that
block third-party cookies fail the check step closed, and browsers that
accept them without honouring `Partitioned` are confined by the
`same-origin` rule above.

Agent origins are siblings of every other ingress host under the apps domain,
so the ingress gateway refuses browser requests that could ride the ingress
cookie from a sibling: on every ingress host an unsafe method or WebSocket
upgrade with `Sec-Fetch-Site: same-site` is rejected (`403
ingress_request_rejected`, counted); workspace hosts also reject `cross-site`,
while public hosts keep accepting cross-site API callers under their own CORS.
An upgrade's `Origin`, when present, must name the request host. Requests
without Fetch Metadata (non-browser clients, `same-origin`, user-initiated
`none`) pass. An Agent origin is never a trusted Console origin: Console
cookie-authenticated writes and the event-stream upgrade reject it with
`403 csrf_denied`, with or without `Sec-Fetch-Site: same-site`. Deploy the apps
domain under a registrable domain separate from the Console and Proxy; when it
shares one, the Proxy warns `agent_origins_share_site` at startup (a last-two-
labels heuristic without the public suffix list) and the SameSite=Strict
Console cookie and these Origin checks are the remaining defence. Agent UI
code on its origin is trusted workspace code: it can act on that Agent as the
user, and this origin does not isolate one Agent's browser code from another
Agent's origin beyond the browser's same-origin policy. Embedding the standalone
origin in the Console is a separate follow-up (#156).

When the Console and Proxy are configured on one origin, Core refuses embedded
UI tunnels with `503 embedded_ui_requires_separate_app_origin`. The Console's
Agent-link fallback supports this same-origin topology: after the user opens an
online Agent, an explicit refusal changes only that Agent's standalone links to
Console navigation. It does not restore embedding or bypass Core's refusal.
A split-origin deployment whose refusal response is unreadable through CORS
keeps the existing standalone link; cross-origin refusal fallback is not a
supported guarantee. Other errors also leave links unchanged. The bounded probe
and cache behavior are documented in [the Web guide](../web/README.md).

For Workspace ingress authenticated with an ingress session, and for the
workspace-authenticated Managed App UI tunnel, the Proxy issues a fresh
60-second human App token on each request. It uses the App's registered
`service_id` as audience and the current `workspace_member_role`, and stops
issuing when membership is revoked. The App receives it in
`X-Treer-Identity-Token`, so **App code can act as the current user through that
service's App APIs until this token expires**. This is intentional delegation
to trusted workspace code, not merely a login hint. Exposure of this forwarded token grants that short-lived, service-scoped
authority; the token itself does not reveal the platform login session or grant
another App's audience. This token scope does not isolate browser-embedded App
JavaScript from the Proxy-origin session authority described above. Apps must verify tokens and current
membership on every request, avoid logging/persisting forwarded tokens, and
apply their own data permissions. The 12-hour OAuth token is never used for
per-request forwarding. Public ingress issues no human identity, even if the
browser presents Treer cookies. Existing Agent service-token forwarding is
unchanged. The authenticated UI tunnel remains a workspace route even for an
App whose separate public ingress is anonymous.

Client `x-treer-*` and `Treer-Authorization` headers are removed before trusted
identity injection, including WebSocket upgrade requests. The embedded tunnel
strips all cookies and gateway authorization; ingress strips its session cookie
and Treer login/admin cookies while retaining App-owned cookies and application
Authorization. Authentication-disabled local mode does not mint human tokens.

Automatic human identity does **not** replace CSRF defenses. Ingress session
cookies are `SameSite=Lax`; cross-site navigations can therefore carry the
session. Apps must check `Origin` and `Sec-Fetch-Site` on state-changing browser
requests, including requests authenticated with an injected human token. Reject
untrusted/null origins and cross-site Fetch Metadata, use non-GET mutations,
require JSON/custom request headers where appropriate, and do not allow broad
credentialed CORS. Origin checks must account for the App ingress origin and
the configured Proxy origin of the embedded tunnel, rather than the rewritten
upstream Host. Non-browser service-token clients may lack these browser headers;
that is not a reason to exempt human browser requests from validation.

### Control-plane browser CSRF

With authentication enabled, Core cookie-authenticated POST/PUT/PATCH/DELETE
requests and WebSocket upgrades require exactly one `Origin` equal to the
configured Console origin. Only the registered App UI, Agent interface UI and
virtual-host HTTP proxy tunnel routes additionally accept the configured Proxy
origin, because their embedded pages issue relative requests from that origin.
The embed-authorization mint route is an ordinary Console-origin route; the
embed session and check routes live on the Agent origin and are governed by
the ingress Fetch Metadata rule, not by this classification.
Classification uses Axum's matched route template, never a user-supplied prefix;
Proxy origin alone cannot authorize ordinary Core management API writes.
Missing, `null`, duplicate and foreign origins are
rejected with `403 csrf_denied`; Fetch Metadata, when present, must not contradict
that origin (`Sec-Fetch-Site` must be `same-origin` or `same-site`). A same-site
subdomain is not a trusted origin. CORS and SameSite cookies alone are not this
check. Administrator routes select only the admin cookie: adding Authorization
does not exempt them. User routes select a syntactically valid Bearer first,
otherwise the session cookie; invalid selected Bearer credentials never fall
back to a cookie. Valid Bearer/service clients do not need browser Origin.

Login, registration, password reset, invitation preview and admin login are
protected before their handlers run. Only the four user login/register/reset
endpoints have a native pre-authentication channel: exactly one recognized
`X-Treer-Client`, no Treer session/admin cookie, no Origin and no Fetch Metadata.
It never exempts authenticated writes or admin login. Native clients carrying a
browser session must use the browser protocol or remove that session first.
OAuth start requires a same-origin JSON POST with `X-Treer-Console: 1`; the old
GET returns an explicit error without creating state. Admin update checks use
POST, rather than a stateful GET. Older browser clients must update together.

Credential-authenticated machine enrollment, Controller/Agent APIs, App runtime
and service-audience-token APIs, identity-token verification, public health and
artifact reads are separate protocols. OAuth callbacks use their single-use
state and browser-cookie binding, rather than requiring Origin from the external
provider. App OAuth and ingress authorization are explicit top-level GET
navigation exceptions; supplied Fetch Mode/Dest must be `navigate`/`document`.
App OAuth requires bounded state, S256 PKCE, an enabled workspace ingress with
an exactly matching scheme/host/port and service audience, and a single-use code
bound to the full redirect and challenge with a five-minute TTL. Apps validate
returned state. Ingress codes separately bind to the enabled hostname/ingress
and current membership, are single-use with a five-minute TTL, and accept only
local return paths. They additionally require the initiating browser's verifier
cookie as described below.

These checks do not apply to arbitrary App ingress traffic: each App remains
responsible for its own browser CSRF protection as described above.

Workspace ingress login codes are bound to a browser handshake. The ingress
hostname generates a random verifier with at least 256 bits of entropy; only
its SHA-256 challenge travels to the Proxy authorization endpoint and is stored
with the five-minute, single-use code. Callback redemption requires the matching
verifier cookie, consumes the code atomically (including missing/wrong verifier
failures), and clears that flow cookie. HTTPS uses a host-only `__Host-` cookie
with `Secure`, `HttpOnly`, `SameSite=Lax`, and `Path=/`, preventing sibling
ingress domains from planting a Domain cookie with a known verifier. The prefix
requires `Path=/`; handshake cookies are stripped before forwarding to Apps.
Up to eight concurrent browser flows per hostname are retained; additional
flows evict the oldest browser-supplied flow cookies. Expired or evicted flows
require starting login again. HTTP development uses an unprefixed cookie and
does not provide transport secrecy or protection against sibling-domain cookie
planting. These checks do not isolate compromised same-origin App code.

Bound codes use a separate PostgreSQL table from legacy unbound codes. Mixed
old/new Proxies fail login closed rather than accepting an unbound code; pending
logins can require restarting after an upgrade. Existing ingress sessions and
service-token clients remain compatible. Authorization and callback navigation
reject contradictory Fetch Metadata when present; this is additional defense,
not a substitute for the verifier. Code-bearing redirects and callbacks use
`Cache-Control: no-store` and `Referrer-Policy: no-referrer`.


Self-hosted Compose gives the updater sidecar the host Docker socket and a
read-only bind of `compose.yaml`. Compromise of that sidecar is host Docker
compromise. Proxy, App, PostgreSQL, and NATS do not mount the socket. Hosted
Railway does not run the sidecar. `/api/admin/update*` require a current
platform admin session; workspace members cannot start an image apply.

Registered endpoints are ordinary sockets reachable from the Controller.
Same-machine processes may reach them outside the Proxy. Explicit forwarding
provides workspace authentication at the Treer entry point, not host firewall
isolation. User-provided containers or separate accounts supply isolation when
required. Agent-owned service mutations cannot select another Agent's identity.
Managed Apps and standalone HTTP services receive platform-assigned domains;
custom hosts are rejected. Workspace access is the default, while explicit
public HTTP/TCP exposure accepts anonymous internet connections. Commands,
arguments, target addresses, and launch snapshots are plaintext configuration;
keep secrets out of argv and profiles.

Workspace members may change an existing Managed App's owned ingress between
`workspace` and `public` access from the App settings UI. This endpoint resolves
the ingress from the App record and does not accept a service, hostname, or
arbitrary ingress identifier from the caller. An App selected as the active
Policy Provider cannot be switched to public HTTP access or published as raw
public TCP: selection, publication and every accepted public-TCP connection
check the active Provider service. PostgreSQL triggers serialize selection and
publication under one advisory lock, so concurrent requests on any Proxy cannot
both succeed. Open public-TCP streams periodically recheck the active Provider
and fail closed on a failed or timed-out check. Revocation is not instantaneous:
bytes already in flight and the interval/query-timeout window remain possible.

## Policy And Rollout

Policy is authoritative only after authentication establishes an immutable
subject and resource scope. A workspace owner may delegate rules to a selected
Managed App, but the Proxy retains authentication, scoping, runtime binding,
Provider selection, and the typed cache-invalidation recovery path. Provider
fetches are bounded loopback requests through the Controller, not arbitrary
Proxy egress. A valid stale bundle is used only for the configured window;
`fail_closed` is the default and `fail_open` is an explicit owner choice. A
missing Provider and missing legacy workspace Policy currently defaults to
the visible `Treer Default` monitor/allow baseline. The owner-only default App
installer is a recovery path: it uses bounded machine uploads, private Managed
App creation, protocol validation, and automatic Provider binding without
consulting delegated Policy. It never downloads executable content on the
machine. New default Policy Apps initialize an administration token locally via
a flag in their startup command. The Proxy uploads only bundled source/assets;
it neither generates nor reads the token, and no token is returned by the
installer or included in startup arguments or provisioning audit records. The
App owns its private token file, rotation and recovery. This does not isolate the
token from other processes with the same OS account or grant workspace owners
automatic OS access. Existing App startup commands are not silently migrated.
Monitor mode computes denials without enforcing them; decision audit
is still absent. The Core Message feature flag is deployment sequencing, not a
security control.

## Hardening Order

1. Replace allow-all defaults with reviewed policies and auditable decisions.
2. Bind provider credentials and runtime actions to explicit owners.
3. Keep external isolation and secret-delivery integration explicit; do not
   grow an owned sandbox implementation.
4. Add credential rotation, retention/deletion workflows, quotas, and incident
   diagnostics.
5. Add signed-channel freshness and downgrade protection to installed updaters;
   signature and binary digest checks are already enforced for Controller updates.

Relevant source boundaries are `crates/treer-proxy/src/auth.rs`,
`identity.rs`, `policy.rs`, `message_store.rs`, `updater.rs`,
`deploy/updater/updater.py`, and the Controller CLI, startup-store, and explicit network modules.

### Managed App email notifications

`POST /api/apps/{service_id}/email` lets a Managed App use the Proxy's fixed
`TREER_EMAIL_FROM` sender (default `service@treer.ai`). It accepts only the
runtime identity owning that App and service, with a service-audience token;
human tokens and unrelated Agents cannot use it. Workspace Policy evaluates
`email.send` against that App's Agent subject and `machine.service` resource.
Deployment Policy must explicitly govern this action; permissive/audit modes
retain their ordinary semantics.

Any syntactically valid recipient email address is admitted, including addresses
outside the workspace's organization. No recipient allowlist is required or
consulted. This is permission to send email, not
permission to read Core Messages. Fixed hourly limits are 20 attempts per App
and 5 per recipient per workspace, shared across Proxy replicas and restarts.
Subject is at most 200 bytes and plain text at most 4096 bytes. Caller HTML,
From, and Reply-To are rejected; HTML is generated from escaped text. Credentials
stay in the Proxy. Body-free PostgreSQL audit rows retain App ID, time,
recipient SHA-256 and sending/sent/failed outcome; hashes are pseudonymous, not
anonymous. Policy denials also use the ordinary Policy audit path.

Queued prompt delivery uses the existing `agent.prompt` authorization. Public
request bodies cannot supply sender identity: the Proxy resolves the verified
caller to an Agent ID and canonical tree path, and the Controller supplies it
only over the registered private AIS channel. Human/operator prompts have no
Agent sender. Queue listing requires `agent.queue.read`; reading or cancelling
one's own submission is allowed to its authenticated Agent sender. Reading
another sender's receipt requires `agent.queue.read`, and cancelling another
sender's pending item requires `agent.queue.cancel`. Ownership comes from the
saved launcher receipt, never from request fields. Machine-scoped callers remain
restricted to their machine. Running turns require abort instead of queue cancel.

Completion notifications are authored by the target launcher's identity and
remain subject to Core Message Policy and workspace membership. They carry
bounded prompt/assistant previews; callers must avoid placing secrets in tasks
intended for other Agents. Durable queue and outbox files are launcher-owned
private state, not Core session storage. Missing queue/notification capabilities
fail explicitly instead of falling back to disruptive terminal insertion.
