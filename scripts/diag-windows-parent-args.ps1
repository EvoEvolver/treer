# Read-only diagnostic: run the detached-parent fixture body under argument and
# redirection variants. Private temporary directory only; booleans and timings.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/update-machine.ps1"
$shell = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
$source = Join-Path $PSScriptRoot 'update-machine.ps1'
$body = @'
param($Source,$Directory,$Shell)
. $Source
$child = Join-Path $Directory 'detached-child.ps1'
$marker = Join-Path $Directory 'detached-complete.txt'
$start = New-UpdateProcess $Shell ('-NoProfile -NonInteractive -File "' + $child + '" -Marker "' + $marker + '"') $Directory
$process = [Diagnostics.Process]::Start($start)
$process.Dispose()
'@
$trivial = 'param($Source,$Directory,$Shell); [IO.File]::WriteAllText((Join-Path $Directory "reached.txt"), "x")'
$variants = @(
    @{ Name = 'fixture-body-fixture-args'; Body = $body; Redirect = $false },
    @{ Name = 'trivial-body-fixture-args'; Body = $trivial; Redirect = $false },
    @{ Name = 'fixture-body-output-redirected'; Body = $body; Redirect = $true },
    @{ Name = 'fixture-body-no-source-dot'; Body = $body.Replace('. $Source', '# skipped'); Redirect = $false }
)
foreach ($variant in $variants) {
    $directory = Join-Path ([IO.Path]::GetTempPath()) ('treer-parent-diag-' + [Guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($directory) | Out-Null
    try {
        [IO.File]::WriteAllText((Join-Path $directory 'detached-child.ps1'), 'param($Marker); Start-Sleep -Milliseconds 750; [IO.File]::WriteAllText($Marker, "passed")')
        $script = Join-Path $directory 'detached-parent.ps1'
        [IO.File]::WriteAllText($script, $variant.Body)
        $arguments = '-NoProfile -NonInteractive -File "' + $script + '" -Source "' + $source + '" -Directory "' + $directory + '" -Shell "' + $shell + '"'
        $start = New-UpdateProcess $shell $arguments $directory
        if ($variant.Redirect) { $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true }
        $watch = [Diagnostics.Stopwatch]::StartNew()
        $process = [Diagnostics.Process]::Start($start)
        $out = $null; $err = $null
        if ($variant.Redirect) { $out = $process.StandardOutput.ReadToEndAsync(); $err = $process.StandardError.ReadToEndAsync() }
        $exited = $process.WaitForExit(15000); $elapsed = $watch.ElapsedMilliseconds
        $code = if ($exited) { $process.ExitCode } else { -1 }
        if (!$exited) { $process.Kill(); $process.WaitForExit() }; $process.Dispose()
        $detail = ''
        if ($variant.Redirect) {
            $detail = ' stdout-bytes=' + $out.Result.Length + ' stderr-bytes=' + $err.Result.Length
            if ($err.Result.Length -gt 0) { $detail += ' stderr-first-line=' + (($err.Result -split "`r?`n")[0] -replace '[A-Za-z]:\\[^ ]*', '<path>') }
        }
        Start-Sleep -Milliseconds 1500
        Write-Output ('variant=' + $variant.Name + ' exited=' + $exited + ' exit=' + $code + ' ms=' + $elapsed + ' reached-marker=' + (Test-Path -LiteralPath (Join-Path $directory 'reached.txt')) + ' child-marker=' + (Test-Path -LiteralPath (Join-Path $directory 'detached-complete.txt')) + $detail)
    } finally { Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue }
}
