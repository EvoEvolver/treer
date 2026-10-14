# Read-only diagnostic: how long does the first cmdlet take in a nested Windows
# PowerShell 5.1 under the fixture's cleared environment, cold and warm? Timings
# and booleans only; private temporary directory.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/update-machine.ps1"
$shell = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
$directory = Join-Path ([IO.Path]::GetTempPath()) ('treer-coldtime-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($directory) | Out-Null
$cmdlet = Join-Path $directory 'cmdlet.ps1'
[IO.File]::WriteAllText($cmdlet, 'param($Marker); $null = Join-Path "a" "b"; [IO.File]::WriteAllText($Marker, "x")')
$cacheDirectory = Join-Path $env:LOCALAPPDATA 'Microsoft/Windows/PowerShell'
Write-Output ('analysis-cache-dir-present-before=' + (Test-Path -LiteralPath $cacheDirectory) + ' analysis-cache-path-env-set=' + [bool]$env:PSModuleAnalysisCachePath + ' module-path-entry-count=' + @($env:PSModulePath -split ';').Count)
try {
    foreach ($run in 1..3) {
        $marker = Join-Path $directory ('m' + $run)
        $start = New-UpdateProcess $shell ('-NoProfile -NonInteractive -File "' + $cmdlet + '" -Marker "' + $marker + '"') $directory
        $start.RedirectStandardError = $true
        $watch = [Diagnostics.Stopwatch]::StartNew()
        $process = [Diagnostics.Process]::Start($start)
        $errorText = $process.StandardError.ReadToEndAsync()
        $exited = $process.WaitForExit(180000); $elapsed = $watch.ElapsedMilliseconds
        $code = if ($exited) { $process.ExitCode } else { -1 }
        if (!$exited) { $process.Kill(); $process.WaitForExit() }; $process.Dispose()
        Write-Output ('run=' + $run + ' exited=' + $exited + ' exit=' + $code + ' ms=' + $elapsed + ' marker=' + (Test-Path -LiteralPath $marker) + ' analysis-cache-dir-present-after=' + (Test-Path -LiteralPath $cacheDirectory) + ' stderr-first-line=' + $(if ($exited -and $errorText.Result) { (($errorText.Result -split "`r?`n")[0] -replace '[A-Za-z]:\\[^ ]*', '<path>') } else { '' }))
    }
} finally { Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue }
