# Same-Holding worker pair publication (#111)

This increment continues the original confirmed preparation after bounded
custody transfer and the separate synchronous sender image release. It uses the
same operation, selected loaded runtime, normal Trust and original connection.
An opaque permit borrowed from the retained released sender checks the loaded
runtime, complete binding/key, confirmed catalog, original job intent and held
transfer record. The coordinator retains the loaded Store writer fence and
checks the original Child. Neither wire receipt nor creator exit is this grant.

The receiver latches the explicit publication attempt before awaiting its
payload. It requires the exact existing transfer contract and live original
creator, verifies claim/job/state/backup/all candidates, and appends a fixed
CREATE_NEW `worker-pair-started.bin`, flushed and read back before any rename.
Evidence includes full binding, original operation/challenge/offer digest,
accepted raw startup snapshot, installed physical IDs captured while original
receiver image pins were held, and complete candidate physical IDs. Phase files
are immutable evidence, not a resume constructor. A conflicting phase refuses
without replacing either installed image.

WorkerReadyPair retains its original independent parents and reduced inherited
installation lease throughout an in-place single attempt. The two fixed-name
renames are deliberately not pair-atomic. First-rename refusal records refusal;
second-rename failure or uncertain observation stops with a mixed/unknown pair
and the remaining guards. There is no guessed compensation, automatic retry or
permission to retire the claim. Explicit verified whole-pair rollback remains a
later transaction boundary.

After both renames, retained identity bridges permit closing the write-capable
candidate file objects and opening fresh restrictive read objects.
Candidate creation permits compatible read-sharing for that bridge while
denying new write/delete access; candidate contents are release images, not
startup secrets. The candidate control now checks exact readable bytes and
denial of writes/deletion instead of the previous share-zero read refusal.
Merely lowering duplicate handle rights would retain the original FileObject sharing
and write access and may prevent executable mapping. The reopen can observe a
changed name or competing access: physical identity checks and all four whole
hashes under final restrictive handles must succeed before publication evidence
or ACK. Any failure retains remaining candidates/bridges/parents/lease in the
existing receiver; it never consumes the object into an error that drops them.

The fixed `worker-pair-published.bin` is created only after whole-pair identity,
signed hash, raw state and backup verification. The original finite transfer
deadline is checked before effects and is never extended. On an error or lost
connection the receiver retains existing guards until that same deadline; it
does not listen again, resume or mutate. Phase flush/readback is not a power-loss
durability guarantee, and enclosing Job/Host/OS termination remains unknown.

New focused selector `windows_update_worker_publication_` covers two portable
sequencing/cancellation controls and four new native controls: normal shared
inventory transfer/release/publication with actual task-owned signed PE
`--version` execution under the retained restrictive pins; refusal without
explicit sender release; a real second-image reader lock leaving mixed unknown
with guards; and a conflicting started phase preserving both old images.
The sole [Windows run 40446825216](https://github.com/EvoEvolver/treer/actions/runs/40446825216)
at `abef5044dd5debbe3be33c4508c3a7b0b52cf4e7` passed all six controls
(portable2/native4, zero failed or ignored, 378 filtered) on
Windows2022/Rust1.90/FixedNTFS. Same-source dev/test builds and test execution
exited zero. Both image hashes/sizes were unchanged; the isolated TEMP was empty
and removed. RESULT SHA256 is
`b377656f7cd7efe570d1a2a3696933842834d1e0e4319f321468998576efa626`.
The actual controls include candidate compatible read-sharing with write/delete
refusal, both published task PE images executing `--version` successfully under
receiver read pins, and mixed/unknown retaining guards after a real CLI reader
lock blocks the second rename. The initial installed fixture bytes are inert;
this does not establish publication over a live mapped Controller image.
PE execution is terminal and neither starts Host nor an installed Controller;
it is not a health check or proof of a running target version serving workloads.
ONE source review's candidate read-sharing test-premise finding was closed in
the same review by aligning the old control and adding that assertion to the
new success case; production bytes were unchanged by the fixture successor.
No old selector, transfer6, matrix or complete gate was rerun.

Remaining complete transaction: compose conditional Controller-only restart
and target health; preserve v2 deletion/denial fences through whole-pair/startup
rollback; persist terminal outcome and reconcile the original loaded owner;
retire the installation claim only after a proven terminal state. Public Windows
capability, deployment, installed processes, Host and publisher remain unchanged.
