# Manual Canary updater. SHA256SUMS is integrity pinning, not a publisher signature.
[CmdletBinding()]
param(
    [ValidateSet('Plan','Prepare','Download','Apply','Status','Worker')][string]$Action = 'Plan',
    [string]$Commit,
    [string]$ConfigPath,
    [string]$BinDirectory,
    [string]$StageDirectory,
    [string]$ArtifactBase = 'https://proxy.canary.treer.ai/artifacts/windows-x86_64/',
    [ValidateSet('treer-agent-server','treer')][string]$Binary,
    [ValidateRange(1,1800)][int]$TimeoutSeconds = 180
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Write-UpdateJson($Path, $Value) {
    $temporary = $Path + '.' + [Guid]::NewGuid().ToString('N') + '.tmp'
    [IO.File]::WriteAllText($temporary, ($Value | ConvertTo-Json -Depth 12), (New-Object Text.UTF8Encoding($false)))
    # File.Replace is atomic on Windows and preserves the private destination ACL.
    if ([IO.File]::Exists($Path)) { [IO.File]::Replace($temporary, $Path, $null) }
    else { [IO.File]::Move($temporary, $Path) }
}
function New-PrivateUpdateDirectory($Path) {
    if (Test-Path -LiteralPath $Path) { throw 'update_directory_exists' }
    $security = New-Object Security.AccessControl.DirectorySecurity
    $security.SetAccessRuleProtection($true, $false)
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    foreach ($principal in @($sid, (New-Object Security.Principal.SecurityIdentifier('S-1-5-18')))) {
        $rule = New-Object Security.AccessControl.FileSystemAccessRule($principal, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
        $security.AddAccessRule($rule)
    }
    # Both shells create with the DACL before any files are written.
    $directory = New-Object IO.DirectoryInfo($Path)
    if ($PSVersionTable.PSVersion.Major -ge 7) { [IO.FileSystemAclExtensions]::Create($directory, $security) }
    else { $directory.Create($security) }
}
function Assert-UpdatePath($Path) {
    $full = [IO.Path]::GetFullPath($Path)
    $item = Get-Item -LiteralPath $full
    while ($null -ne $item) {
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'update_reparse_path_refused' }
        if ($item -is [IO.DirectoryInfo]) { $item = $item.Parent }
        else { $item = $item.Directory }
    }
    return $full
}
function Get-UpdateManifest($Path) {
    if ((Get-Item -LiteralPath $Path).Length -gt 8192) { throw 'invalid_checksum_manifest' }
    $rows = @{}
    foreach ($line in [IO.File]::ReadAllLines($Path)) {
        if ($line -notmatch '^([0-9a-fA-F]{64}) [ *](treer(?:-agent-(?:server|host))?)$') { throw 'invalid_checksum_manifest' }
        if ($rows.ContainsKey($Matches[2])) { throw 'duplicate_checksum_entry' }
        $rows[$Matches[2]] = $Matches[1].ToLowerInvariant()
    }
    if ($rows.Count -ne 3) { throw 'incomplete_checksum_manifest' }
    return $rows
}
function Assert-UpdateOrigin($Base) {
    $uri = $null
    if (![Uri]::TryCreate($Base, [UriKind]::Absolute, [ref]$uri) -or $uri.Scheme -ne 'https' -or
        $uri.UserInfo -or $uri.Query -or $uri.Fragment) { throw 'update_requires_exact_https_origin' }
    return $uri.AbsoluteUri.TrimEnd('/') + '/'
}
function Get-UpdateArtifact($Uri, $Destination, $Limit) {
    # No ambient proxy, redirects, credentials or executable shell interpolation.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $request = [Net.HttpWebRequest]::Create($Uri)
    $request.Proxy = $null; $request.AllowAutoRedirect = $false
    $request.Timeout = 20000; $request.ReadWriteTimeout = 1000
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    $response = $null; $output = $null; $stream = $null
    try {
        $response = $request.GetResponse()
        if ([int]$response.StatusCode -ne 200 -or $response.ContentLength -gt $Limit) { throw 'artifact_response_refused' }
        $stream = $response.GetResponseStream()
        $output = [IO.File]::Open($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        $buffer = New-Object byte[] 65536; [long]$total = 0
        while (($count = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) {
            if ([DateTime]::UtcNow -gt $deadline) { throw 'artifact_download_deadline' }
            $total += $count; if ($total -gt $Limit) { throw 'artifact_size_limit' }
            $output.Write($buffer, 0, $count)
        }
        $output.Flush($true)
    } finally {
        if ($output) { $output.Dispose() }; if ($stream) { $stream.Dispose() }; if ($response) { $response.Dispose() }
    }
}
function New-UpdateProcess($Executable, $Arguments, $WorkingDirectory) {
    $start = New-Object Diagnostics.ProcessStartInfo
    $start.FileName = $Executable; $start.Arguments = $Arguments
    $start.WorkingDirectory = $WorkingDirectory; $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.EnvironmentVariables.Clear()
    foreach ($key in @('SystemRoot','windir','USERPROFILE','USERNAME','USERDOMAIN','APPDATA','LOCALAPPDATA','ProgramData','HOMEDRIVE','HOMEPATH','ComSpec','PATHEXT','PATH','TEMP','TMP')) {
        $value = [Environment]::GetEnvironmentVariable($key)
        if ($value) { $start.EnvironmentVariables[$key] = $value }
    }
    return $start
}
function Get-UpdateVersion($Path) {
    $start = New-UpdateProcess $Path '--version' ([IO.Path]::GetDirectoryName($Path))
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    $process = New-Object Diagnostics.Process; $process.StartInfo = $start
    try {
        $process.Start() | Out-Null
        $stdout = $process.StandardOutput.ReadToEndAsync(); $stderr = $process.StandardError.ReadToEndAsync()
        if (!$process.WaitForExit(10000)) { $process.Kill(); throw 'version_timeout' }
        $value = $stdout.GetAwaiter().GetResult().Trim()
        $null = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0 -or $value.Length -gt 512 -or $value -notmatch '^\S+ \S+ \(([0-9a-f]{7,40})\)$') { throw 'invalid_binary_version' }
        return $Matches[1]
    } finally { $process.Dispose() }
}
function Get-UpdateConfig($Path) {
    $expected = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'Treer/agent-servers'))
    if ([IO.Path]::GetDirectoryName($Path) -ine $expected) { throw 'require_default_local_config_directory' }
    $config = [IO.File]::ReadAllText($Path) | ConvertFrom-Json
    if ($config.install_hostname -ne [Net.Dns]::GetHostName() -or $config.service_manager -ne 'detached') { throw 'require_local_detached_install' }
    $siblings = @(Get-ChildItem -LiteralPath ([IO.Path]::GetDirectoryName($Path)) -Filter '*-controller.json' | Where-Object {
        ([IO.File]::ReadAllText($_.FullName) | ConvertFrom-Json).install_hostname -eq [Net.Dns]::GetHostName()
    })
    if ($siblings.Count -ne 1) { throw 'shared_binary_multi_workspace_refused' }
    $uri = [Uri]('http://' + $config.listen)
    $address = $null
    if (![Net.IPAddress]::TryParse($uri.DnsSafeHost, [ref]$address) -or ![Net.IPAddress]::IsLoopback($address)) { throw 'require_loopback_controller' }
    return $config
}
function Get-UpdateLocal($Config, $Route) {
    $request = [Net.HttpWebRequest]::Create('http://' + $Config.listen + $Route)
    $request.Proxy = $null; $request.AllowAutoRedirect = $false; $request.Timeout = 2000; $request.ReadWriteTimeout = 2000
    if ($Route -eq '/api/local/agents') { $request.Headers['x-treer-operator-credential'] = $Config.operator_credential }
    $response = $null; $reader = $null
    try {
        $response = $request.GetResponse()
        if ([int]$response.StatusCode -ne 200) { throw 'local_health_refused' }
        $reader = New-Object IO.StreamReader($response.GetResponseStream())
        $text = New-Object Text.StringBuilder
        $buffer = New-Object char[] 4096
        while (($count = $reader.Read($buffer,0,$buffer.Length)) -gt 0) {
            if ($text.Length + $count -gt 4194304) { throw 'local_response_limit' }
            $null = $text.Append($buffer,0,$count)
        }
        $value = $text.ToString() | ConvertFrom-Json
        if ($Route -eq '/api/health' -and ($value.service -ne 'treer-agent-server' -or $value.workspace_id -ne $Config.workspace -or $value.server_id -ne $Config.server_id)) { throw 'local_identity_mismatch' }
        return $value
    } finally { if ($reader) { $reader.Dispose() }; if ($response) { $response.Dispose() } }
}
function Get-UpdateAgents($Config) {
    $rows = (Get-UpdateLocal $Config '/api/local/agents').agents
    return (@($rows | ForEach-Object { $_.agent_id + ':' + $_.pid + ':' + $_.run_generation } | Sort-Object) -join "`n")
}
function Test-UpdateCommit($Expected, $Reported) {
    return ($Expected -match '^[0-9a-f]{7,40}$' -and $Reported -match '^[0-9a-f]{7,40}$' -and
        ($Expected.StartsWith($Reported) -or $Reported.StartsWith($Expected)))
}
function Wait-UpdateController($Config, $Epoch, $Expected, $Agents, $Timeout) {
    $end = [DateTime]::UtcNow.AddSeconds($Timeout)
    do {
        try {
            $health = Get-UpdateLocal $Config '/api/health'
            if ($health.proxy_connected -eq $true -and $health.controller_epoch -and $health.controller_epoch -ne $Epoch -and
                (Test-UpdateCommit $Expected $health.controller_build.git_commit) -and
                @($health.cli_capabilities) -contains 'agent.path-targets.v1' -and
                (Get-UpdateAgents $Config) -ceq $Agents) { return $health }
        } catch { } # A disconnect is expected; only a fresh verified health receipt succeeds.
        Start-Sleep -Milliseconds 500
    } while ([DateTime]::UtcNow -lt $end)
    throw 'controller_readiness_or_agent_preservation_failed'
}
function Invoke-UpdateRestart($Controller, $Workspace, $Directory) {
    if ($Workspace -notmatch '^ws_[A-Za-z0-9_-]+$') { throw 'invalid_workspace' }
    $start = New-UpdateProcess $Controller ('service --workspace ' + $Workspace + ' restart-controller') $Directory
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    $process = New-Object Diagnostics.Process; $process.StartInfo = $start
    try {
        $process.Start() | Out-Null
        $stdout = $process.StandardOutput.ReadToEndAsync(); $stderr = $process.StandardError.ReadToEndAsync()
        if (!$process.WaitForExit(15000)) { $process.Kill(); throw 'restart_command_timeout' }
        $null = $stdout.GetAwaiter().GetResult(); $null = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw 'restart_command_failed' }
    } finally { $process.Dispose() }
}
function Set-UpdateBinary($Source, $Destination) {
    $temporary = $Destination + '.' + [Guid]::NewGuid().ToString('N') + '.new'
    $old = $Destination + '.' + [Guid]::NewGuid().ToString('N') + '.old'
    [IO.File]::Copy($Source, $temporary, $false)
    [IO.File]::Move($Destination, $old)
    try { [IO.File]::Move($temporary, $Destination) }
    catch { [IO.File]::Move($old, $Destination); throw }
    # Keep the running image's .old file until explicit maintenance after exit.
}
function Invoke-UpdateTransaction($Job, $Stage) {
    $config = Get-UpdateConfig $Job.config_path
    $bin = Assert-UpdatePath $Job.bin_directory
    $health = Get-UpdateLocal $config '/api/health'
    if (!$health.proxy_connected -or !$health.controller_epoch) { throw 'controller_not_ready_before_update' }
    $agents = Get-UpdateAgents $config
    $hashes = Get-UpdateManifest (Join-Path $Stage 'SHA256SUMS')
    foreach ($name in @('treer-agent-server','treer')) {
        $file = Join-Path $Stage ($name + '.exe')
        if ((Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hashes[$name] -or
            !(($Job.commit).StartsWith((Get-UpdateVersion $file)))) { throw 'staged_binary_mismatch' }
    }
    $controller = Join-Path $bin 'treer-agent-server.exe'
    foreach ($name in @('treer-agent-server.exe','treer.exe','treer-agent-host.exe')) { $null = Assert-UpdatePath (Join-Path $bin $name) }
    $oldCommit = Get-UpdateVersion $controller
    if (!(Test-UpdateCommit $oldCommit $health.controller_build.git_commit)) { throw 'installed_and_running_controller_differ' }
    $backup = Join-Path ([IO.Path]::GetDirectoryName($Stage)) ('backup-' + $oldCommit + '-' + [Guid]::NewGuid().ToString('N'))
    New-PrivateUpdateDirectory $backup
    foreach ($name in @('treer-agent-server','treer')) { [IO.File]::Copy((Join-Path $bin ($name + '.exe')), (Join-Path $backup ($name + '.exe')), $false) }
    Write-UpdateJson (Join-Path $Stage 'progress.json') @{ status='activating'; backup=$backup }
    $changed = New-Object 'Collections.Generic.List[string]'; $activated = $false
    try {
        $changed.Add('treer-agent-server')
        Set-UpdateBinary (Join-Path $Stage 'treer-agent-server.exe') $controller
        $activated = $true
        Invoke-UpdateRestart $controller $config.workspace $Stage
        $null = Wait-UpdateController $config $health.controller_epoch $Job.commit $agents $Job.timeout
        $changed.Add('treer')
        Set-UpdateBinary (Join-Path $Stage 'treer.exe') (Join-Path $bin 'treer.exe')
        foreach ($name in @('treer-agent-server','treer')) {
            if ((Get-FileHash -LiteralPath (Join-Path $bin ($name + '.exe')) -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hashes[$name]) { throw 'installed_checksum_mismatch' }
        }
        if ((Get-UpdateAgents $config) -cne $agents) { throw 'agent_process_changed' }
        Write-UpdateJson (Join-Path $Stage 'result.json') @{ status='complete'; commit=$Job.commit; backup=$backup; host_changed=$false; agents_preserved=$true }
    } catch {
        $failure = 'activation_failed'; $rollback = 'rolled_back'
        try {
            for ($index=$changed.Count-1; $index -ge 0; $index--) {
                $name = $changed[$index]
                Set-UpdateBinary (Join-Path $backup ($name + '.exe')) (Join-Path $bin ($name + '.exe'))
            }
            if ($activated) {
                $epoch = $null
                try { $epoch = (Get-UpdateLocal $config '/api/health').controller_epoch } catch { }
                Invoke-UpdateRestart $controller $config.workspace $Stage
                $null = Wait-UpdateController $config $epoch $oldCommit $agents $Job.timeout
            }
        } catch { $rollback = 'rollback_failed' }
        Write-UpdateJson (Join-Path $Stage 'result.json') @{ status=$rollback; error=$failure; backup=$backup; host_changed=$false }
        throw 'update_activation_failed_inspect_receipt'
    }
}
function Invoke-WindowsMachineUpdate {
    if ($env:OS -ne 'Windows_NT') { throw 'windows_only' }
    if ($Action -eq 'Plan') { @{ status='plan'; changes=$false; order=@('Prepare','Download Controller','Download CLI','Apply detached','Status'); host_changed=$false } | ConvertTo-Json; return }
    if (!$StageDirectory) {
        if ($Commit -notmatch '^[0-9a-f]{40}$' -or !$env:LOCALAPPDATA) { throw 'require_full_commit_and_localappdata' }
        $StageDirectory = Join-Path $env:LOCALAPPDATA ('Treer/stage-' + $Commit)
    }
    $stage = [IO.Path]::GetFullPath($StageDirectory)
    if ($Action -eq 'Prepare') {
        if ($Commit -notmatch '^[0-9a-f]{40}$' -or !$ConfigPath -or !$BinDirectory) { throw 'require_commit_config_and_binary_directory' }
        $configFile = Assert-UpdatePath $ConfigPath; $bin = Assert-UpdatePath $BinDirectory
        $null = Get-UpdateConfig $configFile
        $base = Assert-UpdateOrigin $ArtifactBase
        if (!(Test-Path -LiteralPath ([IO.Path]::GetDirectoryName($stage)))) { throw 'create_user_owned_stage_parent_first' }
        $null = Assert-UpdatePath ([IO.Path]::GetDirectoryName($stage))
        New-PrivateUpdateDirectory $stage
        Get-UpdateArtifact ($base + 'SHA256SUMS') (Join-Path $stage 'SHA256SUMS') 8192
        $null = Get-UpdateManifest (Join-Path $stage 'SHA256SUMS')
        [IO.File]::Copy($PSCommandPath, (Join-Path $stage 'worker.ps1'), $false)
        Write-UpdateJson (Join-Path $stage 'job.json') @{ schema=1; commit=$Commit; config_path=$configFile; bin_directory=$bin; artifact_base=$base; timeout=$TimeoutSeconds }
        @{ status='prepared'; stage=$stage } | ConvertTo-Json; return
    }
    $null = Assert-UpdatePath $stage
    if ($Action -eq 'Status') {
        foreach ($name in @('result.json','progress.json')) {
            $path = Join-Path $stage $name
            if (Test-Path -LiteralPath $path) { Get-Content -LiteralPath $path -Raw; return }
        }
        @{status='prepared';stage=$stage} | ConvertTo-Json; return
    }
    $job = Get-Content -LiteralPath (Join-Path $stage 'job.json') -Raw | ConvertFrom-Json
    if ($job.schema -ne 1 -or $job.commit -notmatch '^[0-9a-f]{40}$') { throw 'invalid_update_job' }
    if ($Action -eq 'Download') {
        if (!$Binary) { throw 'download_requires_binary' }
        $manifest = Get-UpdateManifest (Join-Path $stage 'SHA256SUMS')
        $destination = Join-Path $stage ($Binary + '.exe')
        Get-UpdateArtifact ((Assert-UpdateOrigin $job.artifact_base) + $Binary) $destination 268435456
        if ((Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash.ToLowerInvariant() -ne $manifest[$Binary]) { throw 'download_checksum_mismatch' }
        @{status='downloaded';binary=$Binary} | ConvertTo-Json; return
    }
    if ($Action -eq 'Apply') {
        # File lock is inherited by no child. Worker itself takes the lock before any mutation.
        $worker = Join-Path $stage 'worker.ps1'
        if ($stage.Contains('"') -or $stage.Contains("`r") -or $stage.Contains("`n")) { throw 'invalid_worker_path' }
        $shell = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
        $start = New-UpdateProcess $shell ('-NoProfile -NonInteractive -File "' + $worker + '" -Action Worker -StageDirectory "' + $stage + '"') $stage
        # No inherited stdout/stderr pipe: restarting Controller cannot hold exec open.
        $process = [Diagnostics.Process]::Start($start)
        $workerId = $process.Id; $process.Dispose()
        @{status='submitted';worker_pid=$workerId;stage=$stage} | ConvertTo-Json; return
    }
    if ($Action -eq 'Worker') {
        $lock = $null
        try {
            $lock = [IO.File]::Open((Join-Path $job.bin_directory '.treer-machine-update.lock'), [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
            if (Test-Path -LiteralPath (Join-Path $stage 'result.json')) { throw 'update_already_finished' }
            Write-UpdateJson (Join-Path $stage 'progress.json') @{status='running';worker_pid=$PID}
            Invoke-UpdateTransaction $job $stage
        } catch {
            if ($lock -and !(Test-Path -LiteralPath (Join-Path $stage 'result.json'))) { Write-UpdateJson (Join-Path $stage 'result.json') @{status='failed';error='worker_failed_before_activation'} }
            throw 'update_worker_failed_inspect_receipt'
        } finally { if ($lock) { $lock.Dispose() } }
    }
}
# Dot-sourcing exposes helpers for hermetic native tests without executing updates.
if ($MyInvocation.InvocationName -ne '.') { Invoke-WindowsMachineUpdate }
