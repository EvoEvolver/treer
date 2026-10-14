# Read-only diagnostic: does the first cmdlet (module auto-load) stall a nested
# Windows PowerShell 5.1 started with the update fixture's cleared environment,
# and which restored variable cures it? Private temporary directory; booleans/timings.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/update-machine.ps1"
$shell = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
$directory = Join-Path ([IO.Path]::GetTempPath()) ('treer-cmdlet-diag-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($directory) | Out-Null
$dotnet = Join-Path $directory 'dotnet.ps1'; $cmdlet = Join-Path $directory 'cmdlet.ps1'
[IO.File]::WriteAllText($dotnet, 'param($Marker); [IO.File]::WriteAllText($Marker, "x")')
[IO.File]::WriteAllText($cmdlet, 'param($Marker); $null = Join-Path "a" "b"; [IO.File]::WriteAllText($Marker, "x")')
$systemModules = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/Modules'
$variants = @(
    @{ Name = 'dotnet-only-fixture-env'; Script = $dotnet; Add = @{} },
    @{ Name = 'cmdlet-fixture-env'; Script = $cmdlet; Add = @{} },
    @{ Name = 'cmdlet-plus-psmodulepath'; Script = $cmdlet; Add = @{ PSModulePath = $systemModules } },
    @{ Name = 'cmdlet-plus-program-files'; Script = $cmdlet; Add = @{ ProgramFiles = $env:ProgramFiles; 'ProgramFiles(x86)' = ${env:ProgramFiles(x86)}; ProgramW6432 = $env:ProgramW6432; CommonProgramFiles = $env:CommonProgramFiles } },
    @{ Name = 'cmdlet-plus-processor-and-computer'; Script = $cmdlet; Add = @{ PROCESSOR_ARCHITECTURE = $env:PROCESSOR_ARCHITECTURE; NUMBER_OF_PROCESSORS = $env:NUMBER_OF_PROCESSORS; COMPUTERNAME = $env:COMPUTERNAME; SystemDrive = $env:SystemDrive } },
    @{ Name = 'cmdlet-uncleared-environment'; Script = $cmdlet; Add = $null }
)
try {
    foreach ($variant in $variants) {
        $marker = Join-Path $directory ($variant.Name + '.marker')
        $arguments = '-NoProfile -NonInteractive -File "' + $variant.Script + '" -Marker "' + $marker + '"'
        if ($null -eq $variant.Add) { $start = New-Object Diagnostics.ProcessStartInfo; $start.FileName = $shell; $start.Arguments = $arguments; $start.WorkingDirectory = $directory; $start.UseShellExecute = $false; $start.CreateNoWindow = $true }
        else { $start = New-UpdateProcess $shell $arguments $directory; foreach ($key in $variant.Add.Keys) { if ($variant.Add[$key]) { $start.EnvironmentVariables[$key] = $variant.Add[$key] } } }
        $watch = [Diagnostics.Stopwatch]::StartNew()
        $process = [Diagnostics.Process]::Start($start)
        $exited = $process.WaitForExit(10000); $elapsed = $watch.ElapsedMilliseconds
        $code = if ($exited) { $process.ExitCode } else { -1 }
        if (!$exited) { $process.Kill(); $process.WaitForExit() }; $process.Dispose()
        Write-Output ('variant=' + $variant.Name + ' exited=' + $exited + ' exit=' + $code + ' ms=' + $elapsed + ' marker=' + (Test-Path -LiteralPath $marker))
    }
} finally { Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue }
