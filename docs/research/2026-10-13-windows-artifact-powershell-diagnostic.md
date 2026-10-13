# Windows artifact PowerShell timeout diagnosis

Status: investigation, not a production fix or release approval. Source parent
is the fixed candidate `e3ed8ea99c80df072fd1b509b7def2ea235272ae`.

## Observed evidence

| Item | Independent validation | Full artifact job |
| --- | --- | --- |
| Run | 38479309866 | 38481174982 / job 115583862245 |
| Runner image | windows-2022, 20261004.326.1 | Same |
| Rust | Native MSVC 1.90.0 | Same |
| Shell | PowerShell 7 pwsh | Same |
| Parent PS 5.1 invocation before tests | Version command in toolchain step | No equivalent invocation |
| Cargo target | Private runner-temp target | Checkout target plus rust-cache |
| Prior test suite | platform | platform + runtime + Host, including ConPTY |
| Controller test filter | windows_machine_exec | windows_ |
| Launcher payload preparation | Absent | npm ci and verified payload preparation |
| Result | Strict machine exec 2/2, total 91.31s | First PS version probe timed out at 30025ms |

The failure occurs after the first Node arithmetic probe and before the later
Node exact module-path assertion. Therefore the failure log does not prove that
the timed-out child received the fixed module path. The preceding platform test
passes, but its environment is not a substitute for checking the actual exec
environment. No per-probe timing exists in the successful 91.31s log.

The early full-environment PowerShell invocation is a possible cache/initialization
difference, not a proven cause. Test parallelism, payload/cache setup and filtered
account baseline remain alternatives. There is no evidence justifying a larger
timeout, retries, another broad allowlist or the OS-token reconstruction as a fix.

## Diagnostic contract

The unchanged strict test now checks the initial fixed module path before any
child starts and records only child variable **names**, probe index, expected
synthetic marker, duration, timeout flag and exit code. Its original 30s limit
and exit/output assertions remain intact.

Run the ignored `windows_artifact_powershell_diagnostics` on a new native runner
after the exact artifact preparation steps. Do not add a PowerShell 5.1 warm-up
before this diagnostic. It uses the real Controller exec path with isolated
synthetic local Host transport and a temporary root; no managed process is used.
It compares startup exit, Console version output, Utility `Write-Output`, disabled
autoload, the system absolute PowerShell executable, and RNG. Each diagnostic
has a fixed 5s deadline; a timeout is recorded rather than disguised as success.
The initial Node probe records names and fixed-module equality. Console version
prints only the numeric version; RNG errors print only the exception type.
No environment values, profile paths, exception messages or stderr are logged.
Ignored diagnostics are evidence collection, never the passing release gate.

Release engineering should retain full artifact preparation and compare its
normal `windows_` filter with a serial diagnostic isolation on a separate fresh
runner. A passing serial probe alone does not waive the concurrent artifact gate.
Any production repair requires its own fixed SHA, safety review and fresh full
native gate. No production environment logic, timeout, cwd conversion, token
environment block reading or launcher asset changes are in this diagnostic.

## First diagnostic result and next controlled comparison

Native run 38483146311 used two fresh artifact-prepared runners. Initial Node
module equality was true. Utility output and RNG exceeded the diagnostic 5s cap;
startup exit, Console version, disabled autoload and absolute Console version
completed in 146–183ms (PS 5.1.20348.5622). The strict runner passed but its four
cmdlet-dependent probes took 28255, 19531, 19717 and 19774ms. This is a repeated
slow command/module path, **not evidence of a one-time warm-up cost**. Passing
near the original deadline does not resolve the failed artifact publication.

The next ignored CI-only test is
`windows_artifact_powershell_environment_matrix`. It runs the same combined
Utility-output/RNG script directly through Rust `Command`, first inheriting the
runner's full parent environment, then using the Controller's filtered map.
It also runs through the actual Controller. All controls use the same cwd,
NUL stdin, piped output and original 30s deadline. Each variant runs twice;
timeouts stay explicit and no output values or stderr are printed.

Diagnostic-only add-back variants isolate `PSModuleAnalysisCachePath`,
`PSModulePath`, a PS configuration group, an OS extras group, their combination,
and fixed `PSModuleAnalysisCachePath=NUL` (not an inherited path). Parent/child
profile values are compared only for equality. Module cache existence, parent
directory existence, size-increase booleans and Cryptnet cache existence/entry
presence are recorded before/after; no cache contents are read or paths logged.
Full-parent inheritance is guarded by `CI=true` and `GITHUB_ACTIONS=true` and
is only permitted on the disposable isolated runner, never enrolled machines.
These guards are accidental-use protection, not an authentication boundary.

Microsoft's [Windows PowerShell 5.1 documentation](https://github.com/MicrosoftDocs/PowerShell-Docs/blob/main/reference/5.1/Microsoft.PowerShell.Core/About/about_Windows_PowerShell_5.1.md#module-analysis-cache)
documents the default cache location and the `NUL` cache-disable diagnostic.
Do not turn the restored caller paths or the cache-disable variant into a
production allowlist change based on assumption. The variants run in order and
can affect account caches: repeat on fresh runners if that confounds causality.
No conclusion about the full-parent baseline exists until this native test runs.
