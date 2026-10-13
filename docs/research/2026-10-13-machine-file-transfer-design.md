# Machine file transfer v1

Proposed implementation, 2026-10-13. Next release only; no deployment or machine
restart is part of this task. Existing browser/default-App upload semantics stay
unchanged. This design does not turn Treer into a filesystem sandbox.

## User contract and root

`treer machine put --machine M LOCAL REMOTE [--force] [--mode 0600]` and
`treer machine get --machine M REMOTE LOCAL [--force]` transfer one regular file.
`-` denotes local stdin/stdout; all diagnostics go to stderr. REMOTE is a relative
path beneath the enrolled Host root, not the caller's cwd or workspace tree.
Existing parent directories are required; no recursive creation, directories,
devices or symlinks. `treer machine show M` reports the authenticated root and
effective file-size limit. An absolute path, even one beneath root, is rejected.
The default is 64 MiB, configurable downward/upward by the machine operator with
a protocol hard ceiling; Proxy and Controller enforce the lower advertised limit.

On Windows, a Host enrolled from System32 really has System32 as its root; the
transfer must not silently substitute USERPROFILE. Changing an existing Host root
is an explicit operator migration. Ask release-eng to make new Windows installs
choose an explicit user-owned root rather than the install shell's cwd, with
native tests; do not change existing enrollments in this feature.

## Protocol, routing and streaming

Add `machine.file-transfer.v1` to Proxy discovery, Controller registration and
CLI transport negotiation. Missing support at any hop produces
`file_transfer_unsupported` before local destination creation or remote mutation.
Do not reuse or extend legacy UploadBegin/Chunk/Commit in a way that changes its
meaning. New shared types describe begin, bounded chunk, finish and abort, with
direction, operation ID, path, overwrite, optional mode, byte offset, total size
and SHA-256. Reserve a file-kind field for future archive support; v1 accepts only
`file`. Controller owns transfer handles and cleanup, so no Host protocol change
or Host restart is needed: it uses the enrolled root already supplied by Host.

The thin CLI continues to forward argv for Controller parsing. Add a negotiated
transport instruction to open the explicitly named local source/destination;
never interpret a caller-local path as a Controller-local path. Client file I/O,
hashing and atomic destination handling are transport mechanics, not business
parsing. Unknown instructions on old CLI fail before transfer begins.

Route begin/chunk/finish through authenticated Proxy APIs and existing correlated
Controller commands/NATS routing. Each chunk is at most 64 KiB; exactly one
unacknowledged chunk per transfer provides backpressure. All subsequent requests
carry an opaque handle bound to workspace, machine, original caller, direction
and Controller connection generation; no process-local Proxy session state or
sticky routing is required. Reauthorize each operation and reject cross-caller
handle reuse. Commands use the existing distributed machine-owner routing.
Offsets reject duplicate/out-of-order writes rather than appending twice.
Timeout each command and expire idle handles (30 seconds) plus an overall budget;
limit active transfers per caller/machine and Proxy request sizes. Proxy neither
spools content nor accepts a whole-file JSON body. Restart/reconnect invalidates
handles; v1 retries start afresh and never claim resumable transfer.

Both endpoints calculate SHA-256 over actual bytes. Put supplies its final size
and digest at finish, allowing stdin without pre-reading; Controller commits
only on an exact match. Get returns the Controller's digest/size and CLI compares
before installing LOCAL. For `get ... -`, spool to a private bounded local
temporary file and verify before writing stdout: bytes already emitted cannot be
discarded on a later checksum failure. A get reads an opened regular-file handle;
concurrent source changes that alter bytes/size cause failure, not a false
snapshot guarantee. Hashes detect corruption, not a malicious trusted Proxy.

## Filesystem and authorization boundary

Use one platform file-transfer primitive beneath Controller. Validate components
before I/O: reject empty/dot/parent components, NUL/control characters, absolute
paths, backslashes, drive/UNC/device prefixes, colons/ADS, trailing dot/space,
reserved Windows device basenames (including extensions), and case variants.
Use `/` on the wire on every platform; do not lowercase ordinary filenames.
Unix traversal must retain root/parent directory handles, open components with
no-follow directory flags and perform create/read/rename relative to those
handles. Windows must open without following reparse points and validate retained
parent/file handles, including junctions and volume changes. A lexical check plus
canonicalize followed by an ordinary path open is insufficient against swaps.
Hard links and same-account attackers remain outside a sandbox claim; writes
replace directory entries rather than modifying an existing linked inode.

Put stages a create-new private file in the destination directory, enforces the
limit while writing, syncs/closes it, checks digest, then atomically installs it.
No-force must use an atomic no-replace primitive, not exists()+rename(). Force
must atomically replace a regular file and reject symlink/reparse/directory
targets; Windows must not implement this as remove-then-rename. Default Unix
mode is 0600 independent of umask; explicit mode rejects special/setuid bits.
Windows uses current-user private ACLs; POSIX modes other than 0600 produce a
clear unsupported-mode error. Local get uses equivalent atomic/no-overwrite
semantics. Abort/error/drop removes staging files; exact-prefix stale staging
cleanup on startup handles crashes without deleting arbitrary user files.

Policy actions are `machine.file.write` and new `machine.file.read`, evaluated
against the actual machine in the caller's current workspace. Resource attributes
include normalized `file.path`, direction and requested mode/overwrite; these
are filesystem paths, distinct from Policy's workspace tree path. Current
monitor/allow defaults remain unchanged. Existing machine-target restrictions
and owner recovery behavior remain. Audit success/failure with caller, machine,
path, size, digest and stable error code; never bytes, credential values or
credential-bearing headers. Reading a root credential file is powerful: grant
machine.file.read only to trusted operators and document that risk explicitly.

## Delivery and evidence

1. Commit design; request release-eng supply-chain/platform review and
   security-claude adversarial review. Implement shared capability/wire contracts
   and the handle-relative filesystem primitive with malicious-path tests.
2. Add Controller transfer lifecycle, Proxy authorization/routing/audit, and thin
   CLI transport plus Controller-owned parsing. Add authenticated machine root
   display; coordinate Windows installer root separately with release-eng.
3. Test isolated fake/real Controller round trips, empty/binary/stdin/stdout,
   corruption/limits/permissions, no-overwrite/force/mode, symlink and concurrent
   parent swaps, Windows drive/UNC/device/ADS/case/junction samples, caller/tenant
   handle theft, Policy rejection, old-hop rejection, disconnect/retry cleanup,
   bounded memory/backpressure/timeouts and cross-Proxy routing. Native Windows
   CI must run the filesystem and transfer tests; Mac-only evidence is not a
   Windows acceptance. Update skill, operator guide, architecture/security and
   quality; cross-review then merge local main. No push/deploy/restart.

This is a substantial Core transport/security change, not a wrapper around the
legacy 16 MiB upload. If a platform cannot implement the required safe atomic
primitive, return unsupported on that platform until it can; do not weaken the
contract silently.
