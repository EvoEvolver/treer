# Private Windows receiver trust entry (#111)

This finite source increment starts from delivered `ee94acb5`. It connects the
existing signed whole-input receiver and retained runtime-state inspection to a
separate strict private terminal entry. It does not implement the Windows update
transaction or enable its product capability.

The creator resolves trust using the existing official/explicit/local updater
configuration, checks it against the admitted key, and creates only the original
held signed target image. The child resolves the selected source before reading
the offer. Missing/bad explicit or local keys fail closed; an absent local key on
the exact official base uses the bundled real authority. Neither wire metadata
nor the offer can select a trust key. No network key download or test-key override
is added. The existing legacy terminal ACK remains separate.

Creation errors return the original reservation and error. Its one-shot creation
attempt stays consumed after trust/hash/spawn refusal. The caller must retain
that reservation on an unknown result; dropping it is not proof of child exit.
After actual receipt, the sender retains original pins/lease/claim and runtime
authority. The receiver rechecks its independently held startup snapshot before
explicitly dropping its read-only custody and exiting. A receipt or creator exit
does not authorize original-owner release. This is not a durable custody handoff.

Author controls cover official/custom resolution, priority, missing/invalid/large
keys, changed configuration and strict private arguments. The sole new Windows
selector is `windows_update_worker_entry_`: two portable controls plus two native
controls, with `native-update-acceptance`. Native tests use a separately built
same-source dev Controller via `TREER_TEST_WORKER_ENTRY_BINARY`, a private signed
copy and an explicit synthetic self-hosted public key. They cover actual receipt
and terminal exit while sender resources remain held, plus failed-trust custody,
no replay, malformed argv and missing-key terminal refusal. They do not execute
installed images. Windows results must be recorded separately from author checks.

Actual finite acceptance: sole Windows2022/Rust1.90/FixedNTFS run
[`40390869308`](https://github.com/EvoEvolver/treer/actions/runs/40390869308)
passed on exact source `3efcfe2d91f03f3c34aad9df65b7a5f0fdd93203`, wrapper
`5de7f22b4a92e135d2145102f16db5cbaffc5250`. Dev build, JSON test build and direct
test exits were all zero; four tests passed, none failed/ignored, 326 filtered.
Dev fixture SHA256 was
`c216f4fdd1060ce591f5fe94a656b254a2bdc4544a18c4a213f333140552d711`
(36,402,176 bytes); testexe SHA256 was
`8dc66da4e702c292e0e4885efb369ec0b5fd75dfe77b870bf103421facc19ff5`
(44,272,128 bytes). Both size/hash pairs were unchanged after execution. Private
TEMP had no remaining entries and its empty root was removed. RESULT SHA256:
`951cb51528c57702e6642cc3b2d522435c93bc395e0f926743754716af688624`.
Core independently checked the downloaded result, platform and four test names.
ONE independent Release source review and its same-review comment closure passed.
This evidence remains bound to that tested source, not a later integration SHA.

Remaining full-transaction path:

1. Connect authenticated product/Policy admission and confirmed install/runtime
   selection to these internal APIs and normal release configuration. No caller
   directory, public identity or creator-origin observation grants authority.
2. Establish supported worker survival/Job ownership across the selected
   Controller exit, with an explicit durable custody/parent-release contract.
3. Compose verified image-pair publication with startup CAS/restore, conditional
   Controller-only restart, target identity/health and whole-transaction rollback.
4. Finish durable outcome reconciliation, crash recovery and safe claim retirement
   without replaying unknown operations; preserve startup denial fences.
5. Close signed freshness/high-water and platform acceptance gaps before shipping
   capability. Named machine targets and one executor remain required for any
   real install/restart, separately from source and task-private native evidence.

No Host/live/capability/publisher action is part of this increment. Existing
five-platform published artifacts are available and are not listed as a blocker.
