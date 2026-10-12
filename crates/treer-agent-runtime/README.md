# Agent runtime checks

`cargo test --locked -p treer-agent-runtime` covers output replay, PTY lifecycle,
and terminal mode tracking. On Windows,
`windows_conpty_argv_input_resize_and_stop` uses the same Rust test executable as
an explicitly selected ignored child fixture. Its executable and working
directory contain spaces and Unicode; arguments include literal shell
metacharacters and Unicode. The test checks environment inheritance, waits for
the child's readiness marker before input, checks its input acknowledgement,
resizes and reads back the PTY size, then stops the still-running child and waits
for resource release. It polls the output replay until each required marker
arrives, sharing one 15-second deadline across output assertions. It does not
retry spawning or tolerate missing output. The corresponding Unix test exercises
the same fixture protocol; it does not establish ConPTY correctness.

The fixture intentionally avoids PowerShell/.NET startup. In release run
38207895457, the first attempt produced the cursor query and PowerShell window
titles but missed script output within five seconds; the rerun took 3.84 seconds.
Those titles show output progressed beyond cursor inheritance, and the reader
retains every chunk for replay, so the observed failure is consistent with cold
shell initialization exceeding a test deadline, not evidence of dropped argv
output. Separating the fixture from shell initialization removes that unrelated
source of variability without changing production startup or suppressing the
cursor query. The existing split-query test continues to cover the startup
responder introduced in 33cd99d.

On macOS, `cargo check --locked --all-targets --target x86_64-pc-windows-gnu
-p treer-agent-runtime` and the corresponding `cargo clippy ... -- -D warnings`
are **compilation-only validation**. Only the next native Windows CI run can
validate ConPTY, including the handshake, input, resize, and stop behavior.
