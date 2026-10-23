# Linux deployment binding

The Linux successor combines the accepted Claude adapter with HOME-cwd private
placement parity. No running process, shared installation or Controller changes
are part of this source increment.

Production builds still validate and canonicalize native build inputs. Optional
build-time `TREER_SPIKE_TARGET_CODEX`, `TREER_SPIKE_TARGET_TREER` and
`TREER_SPIKE_TARGET_CLAUDE` bind absolute, normalized deployment paths separately
from those input images. This allows a compatible Linux build host to produce an
artifact for a machine with different account paths and no Rust toolchain.
Runtime provider overrides remain forbidden; fixture features remain excluded.

Before deployment the target manifest must bind source SHA, default features,
ELF architecture/interpreter and libc baseline, artifact/assets hashes, all
embedded paths, and both build-input and target executable hashes/versions.
Target images must match their declared inputs; a path string alone is never
compatibility evidence. Controller updates may change the CLI image, so rebind
its final image only after Release's per-machine handoff. Preserve original
startup and full receipts/session, use a new stopped backup, and retain one
exact original-startup rollback. Installed defaults and running migrations are
reported independently. Windows has no accepted Rust launcher entry here.

Soul wrappers, active background obligations and absent legacy queue state are
not silently flattened into the ordinary migration contract.
