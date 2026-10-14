param([int]$Iterations = 3)
# Read-only diagnostic for the detached-parent update fixture. It reproduces the
# fixture's process shape in a private temporary directory and records timings
# and boolean state only; it never touches an installation, Host or Controller.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/update-machine.ps1"
$shell = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
$parentBody = @'
param($Source,$Directory,$Shell)
function Stamp($Name) { [IO.File]::WriteAllText((Join-Path $Directory ('stamp-' + $Name + '.txt')), [DateTime]::UtcNow.ToString('o')) }
Stamp 'parent-start'
try {
    $state = 'psversion=' + $PSVersionTable.PSVersion + ' psmodulepath-set=' + [bool]$env:PSModulePath + ' profile-file-present=' + [bool](Test-Path -LiteralPath $PROFILE.CurrentUserCurrentHost) + ' user-module-dir-present=' + [bool](Test-Path -LiteralPath (Join-Path $env:USERPROFILE 'Documents/WindowsPowerShell/Modules'))
    [IO.File]::WriteAllText((Join-Path $Directory 'state.txt'), $state)
    . $Source
    Stamp 'sourced'
    $child = Join-Path $Directory 'detached-child.ps1'
    $marker = Join-Path $Directory 'detached-complete.txt'
    $start = New-UpdateProcess $Shell ('-NoProfile -NonInteractive -File "' + $child + '" -Marker "' + $marker + '"') $Directory
    $process = [Diagnostics.Process]::Start($start)
    Stamp 'child-started'
    $process.Dispose()
    Stamp 'parent-end'
} catch { [IO.File]::WriteAllText((Join-Path $Directory 'error.txt'), $_.Exception.GetType().FullName); exit 3 }
'@
function Read-Stamp($Directory, $Name, $Origin) {
    $path = Join-Path $Directory ('stamp-' + $Name + '.txt')
    if (!(Test-Path -LiteralPath $path)) { return 'missing' }
    return [int](([DateTime]::Parse([IO.File]::ReadAllText($path), $null, 'RoundtripKind') - $Origin).TotalMilliseconds)
}
$failed = $false
for ($i = 1; $i -le $Iterations; $i++) {
    $directory = Join-Path ([IO.Path]::GetTempPath()) ('treer-detached-diag-' + [Guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($directory) | Out-Null
    try {
        $cold = New-UpdateProcess $shell '-NoProfile -NonInteractive -Command "exit 0"' $directory
        $watch = [Diagnostics.Stopwatch]::StartNew()
        $probe = [Diagnostics.Process]::Start($cold)
        $probeExited = $probe.WaitForExit(30000); $probeMs = $watch.ElapsedMilliseconds
        $probeCode = if ($probeExited) { $probe.ExitCode } else { -1 }; $probe.Dispose()
        [IO.File]::WriteAllText((Join-Path $directory 'detached-child.ps1'), 'param($Marker); Start-Sleep -Milliseconds 750; [IO.File]::WriteAllText($Marker, "passed")')
        $parentScript = Join-Path $directory 'detached-parent.ps1'
        [IO.File]::WriteAllText($parentScript, $parentBody)
        $source = Join-Path $PSScriptRoot 'update-machine.ps1'
        $args = '-NoProfile -NonInteractive -File "' + $parentScript + '" -Source "' + $source + '" -Directory "' + $directory + '" -Shell "' + $shell + '"'
        $origin = [DateTime]::UtcNow
        $watch.Restart()
        $parent = [Diagnostics.Process]::Start((New-UpdateProcess $shell $args $directory))
        $exited = $parent.WaitForExit(10000); $parentMs = $watch.ElapsedMilliseconds
        $code = if ($exited) { $parent.ExitCode } else { -1 }
        $hasExited = $parent.HasExited
        if (!$hasExited) { $parent.Kill(); $parent.WaitForExit() }; $parent.Dispose()
        $end = [DateTime]::UtcNow.AddSeconds(10)
        $marker = Join-Path $directory 'detached-complete.txt'
        while (!(Test-Path -LiteralPath $marker) -and [DateTime]::UtcNow -lt $end) { Start-Sleep -Milliseconds 100 }
        $state = if (Test-Path -LiteralPath (Join-Path $directory 'state.txt')) { [IO.File]::ReadAllText((Join-Path $directory 'state.txt')) } else { 'state-missing' }
        $ok = $exited -and $code -eq 0 -and (Test-Path -LiteralPath $marker)
        if (!$ok) { $failed = $true }
        Write-Output ('iter=' + $i + ' ok=' + $ok + ' shell-host=' + $PSVersionTable.PSVersion + ' cold-probe-ms=' + $probeMs + ' cold-probe-exit=' + $probeCode + ' parent-timed-out=' + !$exited + ' parent-exit=' + $code + ' parent-ms=' + $parentMs + ' stamps-ms{start=' + (Read-Stamp $directory 'parent-start' $origin) + ' sourced=' + (Read-Stamp $directory 'sourced' $origin) + ' child-started=' + (Read-Stamp $directory 'child-started' $origin) + ' end=' + (Read-Stamp $directory 'parent-end' $origin) + '} error-recorded=' + (Test-Path -LiteralPath (Join-Path $directory 'error.txt')) + ' marker-present=' + (Test-Path -LiteralPath $marker) + ' ' + $state)
    } finally { Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue }
}
if ($failed) { exit 1 }
