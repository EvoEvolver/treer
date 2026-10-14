# Read-only diagnostic: candidate detached-parent fixture shapes under the cleared
# fixture environment. Private temporary directory; timings and booleans only.
param([int]$Iterations = 3)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/update-machine.ps1"
$shell = [IO.Path]::Combine($env:SystemRoot, 'System32/WindowsPowerShell/v1.0/powershell.exe')
$source = [IO.Path]::Combine($PSScriptRoot, 'update-machine.ps1')
$currentParent = @'
param($Source,$Directory,$Shell)
. $Source
$child = Join-Path $Directory 'detached-child.ps1'
$marker = Join-Path $Directory 'detached-complete.txt'
$start = New-UpdateProcess $Shell ('-NoProfile -NonInteractive -File "' + $child + '" -Marker "' + $marker + '"') $Directory
$process = [Diagnostics.Process]::Start($start)
$process.Dispose()
'@
$currentChild = 'param($Marker); Start-Sleep -Milliseconds 750; [IO.File]::WriteAllText($Marker, "passed")'
$neutralParent = @'
param($Source,$Directory,$Shell)
. $Source
$child = $Directory + '\detached-child.ps1'
$marker = $Directory + '\detached-complete.txt'
$start = New-UpdateProcess $Shell ('-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' + $child + '" -Marker "' + $marker + '"') $Directory
$process = [Diagnostics.Process]::Start($start)
$process.Dispose()
'@
$neutralChild = 'param($Marker); [Threading.Thread]::Sleep(750); [IO.File]::WriteAllText($Marker, "passed")'
$variants = @(
    @{ Name = 'current-fixture'; Parent = $currentParent; Child = $currentChild; Policy = '' },
    @{ Name = 'current-plus-policy-bypass'; Parent = $currentParent; Child = $currentChild; Policy = '-ExecutionPolicy Bypass ' },
    @{ Name = 'cmdlet-free-with-policy-bypass'; Parent = $neutralParent; Child = $neutralChild; Policy = '-ExecutionPolicy Bypass ' }
)
$failed = $false
foreach ($variant in $variants) {
    foreach ($i in 1..$Iterations) {
        $directory = [IO.Path]::Combine([IO.Path]::GetTempPath(), 'treer-shape-' + [Guid]::NewGuid().ToString('N'))
        [IO.Directory]::CreateDirectory($directory) | Out-Null
        try {
            [IO.File]::WriteAllText($directory + '\detached-child.ps1', $variant.Child)
            [IO.File]::WriteAllText($directory + '\detached-parent.ps1', $variant.Parent)
            $arguments = '-NoProfile -NonInteractive ' + $variant.Policy + '-File "' + $directory + '\detached-parent.ps1" -Source "' + $source + '" -Directory "' + $directory + '" -Shell "' + $shell + '"'
            $watch = [Diagnostics.Stopwatch]::StartNew()
            $parent = [Diagnostics.Process]::Start((New-UpdateProcess $shell $arguments $directory))
            $exited = $parent.WaitForExit(10000); $parentMs = $watch.ElapsedMilliseconds
            $code = if ($exited) { $parent.ExitCode } else { -1 }
            if (!$exited) { $parent.Kill(); $parent.WaitForExit() }; $parent.Dispose()
            $marker = $directory + '\detached-complete.txt'
            $end = [DateTime]::UtcNow.AddSeconds(10)
            while (![IO.File]::Exists($marker) -and [DateTime]::UtcNow -lt $end) { [Threading.Thread]::Sleep(100) }
            $ok = $exited -and $code -eq 0 -and [IO.File]::Exists($marker)
            if (!$ok) { $failed = $true }
            Write-Output ('variant=' + $variant.Name + ' iter=' + $i + ' ok=' + $ok + ' parent-exited=' + $exited + ' parent-exit=' + $code + ' parent-ms=' + $parentMs + ' worker-marker=' + [IO.File]::Exists($marker))
        } finally { [IO.Directory]::Delete($directory, $true) }
    }
}
if ($failed) { exit 1 }
