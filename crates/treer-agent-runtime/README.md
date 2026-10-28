# Agent runtime checks

`cargo test --locked -p treer-agent-runtime` covers output replay, PTY lifecycle
and terminal mode tracking.

On Windows, `windows_conpty_argv_input_resize_and_stop` runs the test executable
as an ignored child fixture whose path and arguments contain spaces, Unicode and
shell metacharacters. It checks environment inheritance, waits for the child's
readiness marker, checks input acknowledgement, resizes and reads back the PTY
size, then stops the child and waits for resource release, polling replay under
one 15-second deadline. The fixture avoids PowerShell/.NET startup so shell
initialization time cannot cause spurious failures. The Unix test runs the same
fixture protocol but says nothing about ConPTY.

Cross-target `cargo check`/`clippy` for `x86_64-pc-windows-gnu` on macOS is
compilation-only; only the native Windows CI job validates ConPTY.
