# Read-only diagnostic: time-to-first-statement of a nested Windows PowerShell 5.1
# started with the update fixture's cleared environment, under controlled variants.
# Private temporary directory only; records timings and booleans, no environment values.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/update-machine.ps1"
$shell = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
$directory = Join-Path ([IO.Path]::GetTempPath()) ('treer-nested-diag-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($directory) | Out-Null
$script = Join-Path $directory 'first-statement.ps1'
[IO.File]::WriteAllText($script, 'param($Marker); [IO.File]::WriteAllText($Marker, "x")')
Write-Output ('host=' + $PSVersionTable.PSVersion + ' execution-policy-list=' + ((Get-ExecutionPolicy -List | ForEach-Object { $_.Scope.ToString() + ':' + $_.ExecutionPolicy }) -join ','))
$standardModulePath = (Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/Modules')
$variants = @(
    @{ Name = 'fixture-shape'; Args = '-NoProfile -NonInteractive -File "{S}" -Marker "{M}"' },
    @{ Name = 'execution-policy-bypass'; Args = '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "{S}" -Marker "{M}"' },
    @{ Name = 'command-form'; Args = '-NoProfile -NonInteractive -Command "& ''{S}'' -Marker ''{M}''"' },
    @{ Name = 'stdin-closed'; Args = '-NoProfile -NonInteractive -File "{S}" -Marker "{M}"'; Stdin = $true },
    @{ Name = 'module-path-set'; Args = '-NoProfile -NonInteractive -File "{S}" -Marker "{M}"'; ModulePath = $standardModulePath },
    @{ Name = 'uncleared-environment'; Args = '-NoProfile -NonInteractive -File "{S}" -Marker "{M}"'; Uncleared = $true }
)
try {
    foreach ($variant in $variants) {
        $marker = Join-Path $directory ($variant.Name + '.marker')
        $arguments = $variant.Args.Replace('{S}', $script).Replace('{M}', $marker)
        $start = New-UpdateProcess $shell $arguments $directory
        if ($variant.ContainsKey('ModulePath')) { $start.EnvironmentVariables['PSModulePath'] = $variant.ModulePath }
        if ($variant.ContainsKey('Uncleared')) { $start = New-Object Diagnostics.ProcessStartInfo; $start.FileName = $shell; $start.Arguments = $arguments; $start.WorkingDirectory = $directory; $start.UseShellExecute = $false; $start.CreateNoWindow = $true }
        if ($variant.ContainsKey('Stdin')) { $start.RedirectStandardInput = $true }
        $watch = [Diagnostics.Stopwatch]::StartNew()
        $process = [Diagnostics.Process]::Start($start)
        if ($variant.ContainsKey('Stdin')) { $process.StandardInput.Close() }
        $exited = $process.WaitForExit(30000); $elapsed = $watch.ElapsedMilliseconds
        $code = if ($exited) { $process.ExitCode } else { -1 }
        if (!$exited) { $process.Kill(); $process.WaitForExit() }; $process.Dispose()
        Write-Output ('variant=' + $variant.Name + ' exited=' + $exited + ' exit=' + $code + ' ms=' + $elapsed + ' marker-present=' + (Test-Path -LiteralPath $marker))
    }
} finally { Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue }
