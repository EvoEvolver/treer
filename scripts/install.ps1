param(
    [string]$ArtifactBase = '__TREER_ARTIFACT_BASE__',
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Treer\bin')
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ([Environment]::OSVersion.Platform -ne 'Win32NT' -or
    [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
    throw 'This installer requires native Windows x64.'
}
$artifactUri = $null
if ($ArtifactBase -eq '__TREER_ARTIFACT_BASE__' -or
    ![Uri]::TryCreate($ArtifactBase, [UriKind]::Absolute, [ref]$artifactUri) -or
    $artifactUri.Scheme -notin @('http', 'https')) {
    throw 'Use the Proxy /install.ps1 endpoint or supply an HTTP(S) -ArtifactBase URL.'
}
# Hashes come from the same origin, so cleartext HTTP would let a network
# attacker replace both binaries and checksums. Allow it only for loopback tests.
# .NET Framework and modern .NET can format the same IPv6 host differently.
# Compare parsed IPv6 address bytes, without DNS or a wider IPv4 allowlist.
$loopbackOrigin = $artifactUri.Host -in @('localhost', '127.0.0.1')
if ($artifactUri.HostNameType -eq [UriHostNameType]::IPv6) {
    $ipv6Address = $null
    $loopbackOrigin = [Net.IPAddress]::TryParse($artifactUri.DnsSafeHost, [ref]$ipv6Address) -and
        $ipv6Address.Equals([Net.IPAddress]::IPv6Loopback)
}
if ($artifactUri.Scheme -eq 'http' -and !$loopbackOrigin) {
    throw 'Refusing cleartext HTTP artifacts; use an HTTPS Proxy URL (HTTP is allowed only for loopback).'
}
if ($artifactUri.UserInfo) { throw 'ArtifactBase must not contain credentials.' }
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$InstallDir = [IO.Path]::GetFullPath($InstallDir)
$binaries = @('treer', 'treer-launch', 'treer-agent-host', 'treer-agent-server')
foreach ($process in Get-Process) {
    # Only block binaries installed in this directory, including Agent-inherited
    # CLI instances; do not stop unrelated users or another Treer installation.
    try { $path = $process.Path } catch { continue }
    if ($path -and ($binaries + 'treer-agent').Where({ $path -eq (Join-Path $InstallDir "$_.exe") }).Count) {
        throw 'Treer is still running. Stop its workspaces before reinstalling, then restore them afterwards.'
    }
}
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('treer-install-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($temporary) | Out-Null
try {
    $base = $ArtifactBase.TrimEnd('/') + '/windows-x86_64/'
    $checksums = (Invoke-WebRequest -UseBasicParsing -Uri ($base + 'SHA256SUMS')).Content
    if ($checksums -is [byte[]]) { $checksums = [Text.Encoding]::UTF8.GetString($checksums) }
    $hashes = @{}
    foreach ($line in ($checksums -split '\r?\n')) {
        if (!$line.Trim()) { continue }
        if ($line -notmatch '^([0-9a-fA-F]{64})  (treer|treer-launch|treer-agent-host|treer-agent-server)$' -or
            $hashes.ContainsKey($Matches[2])) { throw 'Invalid or duplicate artifact checksum entry.' }
        $hashes[$Matches[2]] = $Matches[1]
    }
    if ($hashes.Count -ne $binaries.Count) { throw 'Incomplete artifact checksum manifest.' }
    $identity = $null
    foreach ($binary in $binaries) {
        $file = Join-Path $temporary "$binary.exe"
        Invoke-WebRequest -UseBasicParsing -Uri ($base + $binary) -OutFile $file
        if ((Get-FileHash -Algorithm SHA256 $file).Hash -ne $hashes[$binary]) {
            throw "Checksum mismatch for $binary. Installation aborted."
        }
        $version = & $file --version
        if ($LASTEXITCODE -ne 0 -or $version -notmatch '^\S+ (\S+) \(([0-9a-fA-F]{7,64})\)$') {
            throw "Invalid build identity for $binary."
        }
        $build = "$($Matches[1])@$($Matches[2])"
        if ($identity -and $identity -ne $build) { throw 'Downloaded binaries belong to different builds.' }
        $identity = $build
    }
    [IO.Directory]::CreateDirectory($InstallDir) | Out-Null
    foreach ($binary in $binaries) {
        Copy-Item -LiteralPath (Join-Path $temporary "$binary.exe") -Destination (Join-Path $InstallDir "$binary.exe") -Force
    }
    # Deprecated alias for profiles saved before the rename; replaces the old Node-era shim.
    Copy-Item -LiteralPath (Join-Path $temporary 'treer-launch.exe') -Destination (Join-Path $InstallDir 'treer-agent.exe') -Force
    Remove-Item -LiteralPath (Join-Path $InstallDir 'treer-agent.exe.json') -Force -ErrorAction SilentlyContinue
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (($userPath -split ';') -notcontains $InstallDir) {
        [Environment]::SetEnvironmentVariable('Path', (($userPath, $InstallDir) | Where-Object { $_ }) -join ';', 'User')
    }
    if (($env:Path -split ';') -notcontains $InstallDir) { $env:Path += ";$InstallDir" }
    Write-Host "Installed Treer $identity in $InstallDir. This shell and newly opened shells can use the Treer commands."
    Write-Host 'For a new machine, create a user-owned workspace directory:'
    Write-Host 'New-Item -ItemType Directory -Force -Path "$env:LOCALAPPDATA\Treer\workspace" | Out-Null'
    Write-Host 'Next: treer-agent-server connect --root "$env:LOCALAPPDATA\Treer\workspace" --key ENROLLMENT_KEY --proxy PROXY_URL'
    Write-Host 'Existing enrolled machines keep their configured root; installation does not migrate or reconnect them.'
} finally {
    Remove-Item -LiteralPath $temporary -Recurse -Force
}
