# Read-only diagnostic: bisect which script argument stalls a nested Windows
# PowerShell 5.1 -File start. Private temporary directory only; booleans and timings.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/update-machine.ps1"
$shell = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
$trivial = 'param($Source,$Directory,$Shell,$Other); [IO.File]::WriteAllText((Join-Path $env:TEMP ("reached-" + $PID + ".txt")), "x")'
$directory = Join-Path ([IO.Path]::GetTempPath()) ('treer-parent-diag-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($directory) | Out-Null
$script = Join-Path $directory 'p.ps1'
[IO.File]::WriteAllText($script, 'param($Source,$Directory,$Shell,$Other); [IO.File]::WriteAllText((Join-Path $Directory "reached.txt"), "x")')
$source = Join-Path $PSScriptRoot 'update-machine.ps1'
$variants = @(
    @{ Name = 'directory-only'; Args = ' -Directory "{D}"' },
    @{ Name = 'source-only'; Args = ' -Source "{SRC}" -Directory "{D}"' },
    @{ Name = 'shell-only'; Args = ' -Shell "{SH}" -Directory "{D}"' },
    @{ Name = 'all-three-simple-values'; Args = ' -Source "a" -Directory "{D}" -Shell "b"' },
    @{ Name = 'all-three-real-values'; Args = ' -Source "{SRC}" -Directory "{D}" -Shell "{SH}"' },
    @{ Name = 'other-param-real-value'; Args = ' -Other "{SH}" -Directory "{D}"' },
    @{ Name = 'directory-with-trailing-flag-like-value'; Args = ' -Directory "{D}" -Other "-x"' }
)
try {
    foreach ($variant in $variants) {
        $reached = Join-Path $directory 'reached.txt'
        Remove-Item -LiteralPath $reached -ErrorAction SilentlyContinue
        $arguments = '-NoProfile -NonInteractive -File "' + $script + '"' + $variant.Args.Replace('{D}', $directory).Replace('{SRC}', $source).Replace('{SH}', $shell)
        $watch = [Diagnostics.Stopwatch]::StartNew()
        $process = [Diagnostics.Process]::Start((New-UpdateProcess $shell $arguments $directory))
        $exited = $process.WaitForExit(10000); $elapsed = $watch.ElapsedMilliseconds
        $code = if ($exited) { $process.ExitCode } else { -1 }
        if (!$exited) { $process.Kill(); $process.WaitForExit() }; $process.Dispose()
        Write-Output ('variant=' + $variant.Name + ' exited=' + $exited + ' exit=' + $code + ' ms=' + $elapsed + ' reached=' + (Test-Path -LiteralPath $reached))
    }
} finally { Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue }
