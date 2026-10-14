# Read-only diagnostic: delta-debug the smallest set of extra environment variable
# names that lets a nested Windows PowerShell 5.1 run its first cmdlet when started
# with the update fixture's cleared environment. Outputs variable names only.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/update-machine.ps1"
$shell = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
$directory = Join-Path ([IO.Path]::GetTempPath()) ('treer-envbisect-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($directory) | Out-Null
$cmdlet = Join-Path $directory 'cmdlet.ps1'
[IO.File]::WriteAllText($cmdlet, 'param($Marker); $null = Join-Path "a" "b"; [IO.File]::WriteAllText($Marker, "x")')
$base = New-UpdateProcess 'unused' '' $directory
$extras = @([Environment]::GetEnvironmentVariables().Keys | Where-Object { !$base.EnvironmentVariables.ContainsKey($_) } | Sort-Object)
$counter = 0
function Test-Set($Names) {
    $script:counter++
    $marker = Join-Path $directory ('m' + $script:counter)
    $start = New-UpdateProcess $shell ('-NoProfile -NonInteractive -File "' + $cmdlet + '" -Marker "' + $marker + '"') $directory
    foreach ($name in $Names) { $start.EnvironmentVariables[$name] = [Environment]::GetEnvironmentVariable($name) }
    $process = [Diagnostics.Process]::Start($start)
    $ok = $process.WaitForExit(5000) -and $process.ExitCode -eq 0 -and (Test-Path -LiteralPath $marker)
    if (!$process.HasExited) { $process.Kill(); $process.WaitForExit() }; $process.Dispose()
    return $ok
}
try {
    Write-Output ('extra-variable-count=' + $extras.Count + ' all-extras-cure=' + (Test-Set $extras) + ' no-extras-cure=' + (Test-Set @()))
    $set = @($extras); $chunks = 2
    while ($set.Count -ge 2) {
        $size = [Math]::Max(1, [int][Math]::Ceiling($set.Count / $chunks)); $reduced = $false
        for ($offset = 0; $offset -lt $set.Count; $offset += $size) {
            $without = @($set | Select-Object -First $offset) + @($set | Select-Object -Skip ($offset + $size))
            if ($without.Count -gt 0 -and (Test-Set $without)) { $set = $without; $chunks = [Math]::Max(2, $chunks - 1); $reduced = $true; break }
        }
        if (!$reduced) { if ($size -eq 1) { break }; $chunks = [Math]::Min($set.Count, $chunks * 2) }
    }
    Write-Output ('minimal-curing-names=' + ($set -join ',') + ' trials=' + $counter)
} finally { Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue }
