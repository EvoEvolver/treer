param([string]$ArtifactDirectory)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. "$PSScriptRoot/update-machine.ps1"
function Assert-UpdateTest($Condition, $Label) { if (!$Condition) { throw ('update_test_failed:' + $Label) } }
function Assert-UpdateThrows($Operation, $Label) {
    $failed = $false
    try { & $Operation | Out-Null } catch { $failed = $true }
    Assert-UpdateTest $failed $Label
}
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('treer-update-test-' + [Guid]::NewGuid().ToString('N'))
if ($env:OS -ne 'Windows_NT' -and ($temporary.StartsWith('/var/') -or $temporary.StartsWith('/tmp/'))) { $temporary = '/private' + $temporary }
[IO.Directory]::CreateDirectory($temporary) | Out-Null
try {
    $errors = $null; $tokens = $null
    [Management.Automation.Language.Parser]::ParseFile("$PSScriptRoot/update-machine.ps1", [ref]$tokens, [ref]$errors) | Out-Null
    Assert-UpdateTest ($errors.Count -eq 0) 'parser'
    $receipt = Join-Path $temporary 'progress.json'
    Write-UpdateJson $receipt @{status='running'}
    Write-UpdateJson $receipt @{status='activating'}
    Assert-UpdateTest (([IO.File]::ReadAllText($receipt) | ConvertFrom-Json).status -ceq 'activating') 'replace-existing-progress'
    Write-UpdateJson $receipt @{status='complete'}
    Assert-UpdateTest (([IO.File]::ReadAllText($receipt) | ConvertFrom-Json).status -ceq 'complete') 'replace-progress-again'
    Assert-UpdateTest (@(Get-ChildItem -LiteralPath $temporary -Filter 'progress.json.*.tmp').Count -eq 0) 'no-receipt-temp-leftovers'
    Assert-UpdateTest (Test-UpdateCommit ('a' * 40) ('a' * 7)) 'short-commit-match'
    Assert-UpdateTest (Test-UpdateCommit ('a' * 7) ('a' * 40)) 'rollback-full-health-match'
    Assert-UpdateTest (!(Test-UpdateCommit ('a' * 40) '')) 'empty-health-commit-refused'
    Assert-UpdateTest (!(Test-UpdateCommit ('a' * 40) ('b' * 40))) 'different-health-commit-refused'
    $sum = Join-Path $temporary 'SHA256SUMS'
    $digest = 'a' * 64
    [IO.File]::WriteAllText($sum, "$digest  treer`n$digest  treer-agent-server`n$digest  treer-agent-host`n")
    $pinnedSum = (Get-FileHash -LiteralPath $sum -Algorithm SHA256).Hash
    Assert-UpdateSums $sum $pinnedSum
    Assert-UpdateThrows { Assert-UpdateSums $sum ('b' * 64) } 'out-of-band-manifest-digest'
    Assert-UpdateTest ((Get-UpdateManifest $sum).Count -eq 3) 'manifest-three-binaries'
    [IO.File]::AppendAllText($sum, "$digest  treer`n")
    Assert-UpdateThrows { Get-UpdateManifest $sum } 'duplicate-manifest'
    foreach ($url in @('http://localhost/a/', 'http://fixture.invalid/', 'https://user:secret@fixture.invalid/', 'https://fixture.invalid/?token=secret', 'https://fixture.invalid/#fragment')) {
        Assert-UpdateThrows { Assert-UpdateOrigin $url } 'unsafe-origin'
    }
    Assert-UpdateTest ((Assert-UpdateOrigin 'https://fixture.invalid/artifacts') -ceq 'https://fixture.invalid/artifacts/') 'https-origin'
    $env:TREER_TEST_SYNTHETIC = 'synthetic-not-forwarded'
    $env:NODE_OPTIONS = '--require=synthetic-not-forwarded'
    $start = New-UpdateProcess 'unused' '' $temporary
    Assert-UpdateTest (!$start.EnvironmentVariables.ContainsKey('TREER_TEST_SYNTHETIC')) 'no-workload-env'
    Assert-UpdateTest (!$start.EnvironmentVariables.ContainsKey('NODE_OPTIONS')) 'no-loader-env'
    $null = Remove-Item Env:TREER_TEST_SYNTHETIC,Env:NODE_OPTIONS
    $lockPath = Join-Path $temporary 'exclusive.lock'
    $lockHandle = [IO.File]::Open($lockPath,[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
    try { Assert-UpdateThrows { [IO.File]::Open($lockPath,[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None) } 'exclusive-update-lock' }
    finally { $lockHandle.Dispose() }
    $source = Join-Path $temporary 'source'; $target = Join-Path $temporary 'target'
    [IO.File]::WriteAllText($source,'new'); [IO.File]::WriteAllText($target,'old')
    Set-UpdateBinary $source $target (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
    Assert-UpdateTest ([IO.File]::ReadAllText($target) -ceq 'new') 'rename-and-copy'
    Assert-UpdateThrows { Set-UpdateBinary $source $target ('b' * 64) } 'copy-digest-before-move'
    Assert-UpdateTest ([IO.File]::ReadAllText($target) -ceq 'new') 'copy-digest-keeps-installed-target'
    $oldFiles = @(Get-ChildItem $temporary -Filter 'target.prev-rollback-*')
    Assert-UpdateTest ($oldFiles.Count -eq 1 -and [IO.File]::ReadAllText($oldFiles[0].FullName) -ceq 'old') 'old-image-retained'
    # Pure transaction fixtures: real hashes, file moves, backups, atomic receipts;
    # only enrolled health, versions and restart are replaced. No user's service.
    $script:events = New-Object 'Collections.Generic.List[string]'
    $script:failCandidate = $false; $script:failRollback = $false
    $script:oldCommit = '1' * 40; $script:newCommit = '2' * 40
    function Get-UpdateConfig($Path) { return [PSCustomObject]@{workspace='ws_fixture';listen='127.0.0.1:1';bin_directory=$script:fixtureBin} }
    function Get-UpdateLocal($Config,$Route) {
        return [PSCustomObject]@{proxy_connected=$true;controller_epoch='before';controller_build=[PSCustomObject]@{git_commit=$script:oldCommit};host_build=[PSCustomObject]@{git_commit=$script:oldCommit}}
    }
    function Get-UpdateAgents($Config) { return 'agent-fixture:123:7' }
    function Get-UpdateVersion($Path) {
        if ([IO.File]::ReadAllText($Path).StartsWith('new')) { return $script:newCommit }
        return $script:oldCommit
    }
    function New-PrivateUpdateDirectory($Path) { [IO.Directory]::CreateDirectory($Path) | Out-Null }
    function Invoke-UpdateRestart($Controller,$Workspace,$Directory) { $script:events.Add('restart') }
    function Wait-UpdateController($Config,$Epoch,$Expected,$Agents,$Timeout) {
        $script:events.Add('ready:' + $Expected)
        if ($Expected -eq $script:newCommit) {
            Assert-UpdateTest ([IO.File]::ReadAllText((Join-Path $Config.bin_directory 'treer.exe')).StartsWith('old')) 'cli-not-replaced-before-controller-ready'
        }
        if (($Expected -eq $script:newCommit -and $script:failCandidate) -or ($Expected -eq $script:oldCommit -and $script:failRollback)) { throw 'synthetic-readiness-failure' }
        Assert-UpdateTest ($Agents -ceq 'agent-fixture:123:7') 'inventory-snapshot'
        return [PSCustomObject]@{controller_epoch='after';host_build=[PSCustomObject]@{git_commit=$script:oldCommit}}
    }
    foreach ($scenario in @('complete','rolled_back','rollback_failed','checksum_refused')) {
        $script:events.Clear()
        $case = Join-Path $temporary $scenario
        $stage = Join-Path $case 'stage'; $bin = Join-Path $case 'bin'
        [IO.Directory]::CreateDirectory($stage) | Out-Null; [IO.Directory]::CreateDirectory($bin) | Out-Null
        [IO.File]::WriteAllText((Join-Path $bin 'treer-agent-host.exe'),'old-host')
        $checksums = @()
        foreach ($name in @('treer-agent-server','treer')) {
            [IO.File]::WriteAllText((Join-Path $stage ($name+'.exe')),('new-' + $name))
            [IO.File]::WriteAllText((Join-Path $bin ($name+'.exe')),('old-' + $name))
            $hash = (Get-FileHash -LiteralPath (Join-Path $stage ($name+'.exe')) -Algorithm SHA256).Hash.ToLowerInvariant()
            $checksums += "$hash  $name"
        }
        $checksums += (($digest) + '  treer-agent-host')
        [IO.File]::WriteAllText((Join-Path $stage 'SHA256SUMS'),($checksums -join "`n"))
        $script:failCandidate = $scenario -in @('rolled_back','rollback_failed')
        $script:failRollback = $scenario -eq 'rollback_failed'
        if ($scenario -eq 'checksum_refused') { [IO.File]::AppendAllText((Join-Path $stage 'treer.exe'),'tampered') }
        $script:fixtureBin = $bin
        $startup = Join-Path $case 'startup.json'
        [IO.File]::WriteAllText($startup,'synthetic-startup-intent')
        $job = [PSCustomObject]@{config_path='fixture';bin_directory=$bin;commit=$script:newCommit;sums_sha256=(Get-FileHash -LiteralPath (Join-Path $stage 'SHA256SUMS') -Algorithm SHA256).Hash;timeout=1;startup_store=$startup}
        if ($scenario -eq 'complete') { Invoke-UpdateTransaction $job $stage }
        else { Assert-UpdateThrows { Invoke-UpdateTransaction $job $stage } ('transaction-' + $scenario) }
        if ($scenario -eq 'checksum_refused') {
            Assert-UpdateTest ($script:events.Count -eq 0 -and [IO.File]::ReadAllText((Join-Path $bin 'treer-agent-server.exe')).StartsWith('old')) 'checksum-before-mutation'
        } else {
            $receipt = Get-Content -LiteralPath (Join-Path $stage 'result.json') -Raw | ConvertFrom-Json
            Assert-UpdateTest ($receipt.status -ceq $scenario -and !$receipt.host_changed) ('receipt-' + $scenario)
            $prefix = if ($scenario -eq 'complete') {'new'} else {'old'}
            foreach ($name in @('treer-agent-server','treer')) {
                Assert-UpdateTest ([IO.File]::ReadAllText((Join-Path $bin ($name+'.exe'))).StartsWith($prefix)) ('restored-or-updated-' + $name)
            }
            Assert-UpdateTest ($script:events[0] -ceq 'restart' -and $script:events[1] -ceq ('ready:' + $script:newCommit)) 'controller-before-cli'
            $backup = Get-Item -LiteralPath $receipt.backup
            Assert-UpdateTest ([IO.File]::ReadAllText((Join-Path $backup.FullName 'treer.exe')).StartsWith('old')) 'backup-retained'
            Assert-UpdateTest ([IO.File]::ReadAllText((Join-Path $backup.FullName 'agent-startup.json')) -ceq 'synthetic-startup-intent') 'startup-store-backup'
        }
    }
    if ($env:OS -eq 'Windows_NT') {
        Assert-UpdateTest ($start.EnvironmentVariables['OS'] -ceq 'Windows_NT') 'worker-windows-baseline'
        # Test the actual image lock, not a text-file approximation.
        $image = Join-Path $temporary 'running-shell.exe'
        Copy-Item -LiteralPath (Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe') -Destination $image
        $runningStart = New-UpdateProcess $image '-NoProfile -NonInteractive -Command "Start-Sleep -Seconds 30"' $temporary
        $running = [Diagnostics.Process]::Start($runningStart)
        try {
            Set-UpdateBinary (Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe') $image (Get-FileHash -LiteralPath $image -Algorithm SHA256).Hash
            Assert-UpdateTest (!$running.HasExited -and (Test-Path -LiteralPath $image)) 'running-image-rename'
        } finally { if (!$running.HasExited) { $running.Kill(); $running.WaitForExit() }; $running.Dispose() }
        if (!$ArtifactDirectory) { throw 'native_update_tests_require_real_release_artifacts' }
        # Restore the real version helper, obscured above only in transaction fixtures.
        . "$PSScriptRoot/update-machine.ps1"
        foreach ($name in @('treer-agent-server','treer')) {
            $executable = Join-Path $temporary ($name + '.exe')
            Copy-Item -LiteralPath (Join-Path $ArtifactDirectory $name) -Destination $executable
            Assert-UpdateTest ((Get-UpdateVersion $executable) -match '^[0-9a-f]{7,40}$') 'native-version-child-env'
        }
        $private = Join-Path $temporary 'private'
        New-PrivateUpdateDirectory $private
        $acl = Get-Acl -LiteralPath $private
        Assert-UpdateTest $acl.AreAccessRulesProtected 'private-dacl'
        $current = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
        foreach ($rule in $acl.Access) {
            $sid = $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value
            Assert-UpdateTest ($sid -in @($current,'S-1-5-18')) 'no-other-principal'
        }
        # Native lifetime check: the independently launched worker finishes after
        # its launcher exits; no redirected parent output pipe is retained.
        $childScript = Join-Path $temporary 'detached-child.ps1'
        $parentScript = Join-Path $temporary 'detached-parent.ps1'
        $marker = Join-Path $temporary 'detached-complete.txt'
        # Neither fixture script calls a cmdlet: a nested Windows PowerShell 5.1 started
        # with the cleared update environment spends seconds in module discovery on
        # the first cmdlet, which would measure runner speed instead of lifetime.
        [IO.File]::WriteAllText($childScript, 'param($Marker); [Threading.Thread]::Sleep(750); [IO.File]::WriteAllText($Marker, "passed")')
        [IO.File]::WriteAllText($parentScript, @'
param($Source,$Directory,$Shell)
. $Source
$child = $Directory + '\detached-child.ps1'
$marker = $Directory + '\detached-complete.txt'
$start = New-UpdateProcess $Shell ('-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' + $child + '" -Marker "' + $marker + '"') $Directory
$process = [Diagnostics.Process]::Start($start)
$process.Dispose()
'@)
        $shell = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
        $parentStart = New-UpdateProcess $shell ('-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' + $parentScript + '" -Source "' + $PSScriptRoot + '/update-machine.ps1" -Directory "' + $temporary + '" -Shell "' + $shell + '"') $temporary
        $parent = [Diagnostics.Process]::Start($parentStart)
        try { Assert-UpdateTest ($parent.WaitForExit(10000) -and $parent.ExitCode -eq 0) 'detached-parent-exits' }
        finally { if (!$parent.HasExited) { $parent.Kill(); $parent.WaitForExit() }; $parent.Dispose() }
        $end = [DateTime]::UtcNow.AddSeconds(10)
        while (!(Test-Path -LiteralPath $marker) -and [DateTime]::UtcNow -lt $end) { Start-Sleep -Milliseconds 100 }
        Assert-UpdateTest ((Test-Path -LiteralPath $marker) -and [IO.File]::ReadAllText($marker) -ceq 'passed') 'worker-survives-parent-exit'
        $jobRejected = $false
        try { Assert-UpdateOutsideJob } catch {
            if ($_.Exception.Message -cne 'activation_requires_independent_local_terminal_outside_windows_job') { throw }
            $jobRejected = $true
        }
        $actuallyInJob = $false
        Assert-UpdateTest ([TreerUpdateJobGuard]::IsProcessInJob([TreerUpdateJobGuard]::GetCurrentProcess(), [IntPtr]::Zero, [ref]$actuallyInJob)) 'native-job-query'
        Assert-UpdateTest ($jobRejected -eq $actuallyInJob) 'native-job-activation-guard'
        Write-Output 'windows-update-native-passed' 
    } else { Write-Output 'update-portable-fixtures-passed; native Windows tests not executed' }
} finally {
    Remove-Item -LiteralPath $temporary -Recurse -Force
}
