<#
.SYNOPSIS
Native check that Treer works for a standard (non-administrator) Windows
account, for a DISPOSABLE GitHub-hosted runner only.

Creates a temporary standard local user and, as that user: installs the staged
release artifacts with install.ps1 from a loopback HTTP origin, runs the
installed `treer-agent-server service status`, and runs the given native test
executables (named pipes, ConPTY, Job Objects, private directories, the
registration lock and the detached Host/Controller lifecycle). Every step must
exit 0. The user, its profile and the staging directory are removed afterwards.

Guards: refuses to run unless TREER_DISPOSABLE_RUNNER=1 AND GITHUB_ACTIONS=true
AND RUNNER_ENVIRONMENT=github-hosted (self-hosted runners such as win_native
report "self-hosted"), so it can never create accounts on a real machine.

.PARAMETER InstallScript      Path to install.ps1 under test.
.PARAMETER ArtifactDirectory  Staged release directory (release-artifacts/windows-x86_64).
.PARAMETER TestExecutable     libtest executables run in full as the standard user.
.PARAMETER AgentServerTest    treer-agent-server bin test executable; its registration
                              lock tests and the ignored real Host test run as the
                              standard user (the real Host test needs the debug
                              treer-agent-server.exe and treer-agent-host.exe next
                              to its deps directory).
#>
param(
    [Parameter(Mandatory)][string]$InstallScript,
    [Parameter(Mandatory)][string]$ArtifactDirectory,
    [string[]]$TestExecutable = @(),
    [string]$AgentServerTest
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:TREER_DISPOSABLE_RUNNER -ne '1' -or $env:GITHUB_ACTIONS -ne 'true' -or
    $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Refusing: this check creates a local user and runs only on a disposable GitHub-hosted runner (TREER_DISPOSABLE_RUNNER=1, GITHUB_ACTIONS=true, RUNNER_ENVIRONMENT=github-hosted).'
}
# Secondary Logon is what Start-Process -Credential relies on; say so plainly
# instead of failing later with an opaque error.
$secondaryLogon = Get-Service -Name seclogon -ErrorAction SilentlyContinue
if (-not $secondaryLogon -or $secondaryLogon.Status -ne 'Running') {
    try { Start-Service -Name seclogon -ErrorAction Stop } catch {
        throw 'The Secondary Logon service (seclogon) is not running; Start-Process -Credential cannot run the standard-user checks.'
    }
}
$ArtifactDirectory = [IO.Path]::GetFullPath($ArtifactDirectory)
$random = [System.Security.Cryptography.RandomNumberGenerator]::Create()
$nameBytes = New-Object byte[] 4
$passwordBytes = New-Object byte[] 32
$random.GetBytes($nameBytes)
$random.GetBytes($passwordBytes)
$random.Dispose()
$userName = 'treerstd' + ([BitConverter]::ToString($nameBytes) -replace '-', '').ToLower()
$passwordPlain = [Convert]::ToBase64String($passwordBytes) + 'aA1!'
$password = ConvertTo-SecureString $passwordPlain -AsPlainText -Force
$credential = [PSCredential]::new(".\$userName", $password)
$failures = @()
$staging = Join-Path $env:SystemDrive ('treer-standard-check-' + $userName)
$powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$server = $null

# Runs $Command (PowerShell source) as the standard user. The per-user folders
# are taken from the user's own token, never inherited from the runner.
function Invoke-AsStandardUser {
    param([string]$Label, [string]$Command)
    $prelude = @'
$ErrorActionPreference = 'Stop'
$env:USERPROFILE = [Environment]::GetFolderPath('UserProfile')
$env:LOCALAPPDATA = [Environment]::GetFolderPath('LocalApplicationData')
$env:APPDATA = [Environment]::GetFolderPath('ApplicationData')
$env:TEMP = Join-Path $env:LOCALAPPDATA 'Temp'
$env:TMP = $env:TEMP
[IO.Directory]::CreateDirectory($env:TEMP) | Out-Null
'@
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($prelude + "`n" + $Command + "`nexit `$LASTEXITCODE"))
    $stdout = [IO.Path]::GetTempFileName()
    $stderr = [IO.Path]::GetTempFileName()
    try {
        # Windows PowerShell 5.1 rejects -NoNewWindow together with -Credential.
        $process = Start-Process -FilePath $powershell -Credential $credential -LoadUserProfile `
            -ArgumentList @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', $encoded) `
            -WorkingDirectory $staging -WindowStyle Hidden -Wait -PassThru `
            -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        $output = (Get-Content -Raw $stdout -ErrorAction SilentlyContinue) + (Get-Content -Raw $stderr -ErrorAction SilentlyContinue)
        $exit = $process.ExitCode
    } finally {
        Remove-Item -Force -ErrorAction SilentlyContinue $stdout, $stderr
    }
    Write-Host "::group::[$Label] exit=$exit"
    Write-Host $output
    Write-Host '::endgroup::'
    if ($exit -ne 0) { $script:failures += "$Label exited $exit" }
    return $output
}
function Quote([string]$Value) { "'" + ($Value -replace "'", "''") + "'" }

New-LocalUser -Name $userName -Password $password -AccountNeverExpires -PasswordNeverExpires `
    -Description 'Treer disposable standard-user check' | Out-Null
try {
    Add-LocalGroupMember -Group 'Users' -Member $userName
    if (Get-LocalGroupMember -Group 'Administrators' | Where-Object { $_.Name -like "*\$userName" }) {
        throw 'the check user must not be an administrator'
    }
    # The runner workspace is not readable by a new user: stage what the user
    # runs with read+execute for that user only.
    [IO.Directory]::CreateDirectory($staging) | Out-Null
    & icacls.exe $staging /inheritance:r /grant:r "${userName}:(OI)(CI)RX" /grant:r '*S-1-5-32-544:(OI)(CI)F' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'failed to restrict the staging directory' }
    $stagedScript = Join-Path $staging 'install.ps1'
    Copy-Item -LiteralPath $InstallScript -Destination $stagedScript
    $tests = @($TestExecutable) + @($AgentServerTest | Where-Object { $_ })
    foreach ($directory in @($tests | ForEach-Object { Split-Path -Parent (Split-Path -Parent ([IO.Path]::GetFullPath($_))) } | Sort-Object -Unique)) {
        # target\<profile>: the test executables in deps\ and their sibling binaries.
        & icacls.exe $directory /grant "${userName}:(OI)(CI)RX" /T /C /Q | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "failed to grant read access to $directory" }
    }

    # Serve the staged artifacts over loopback HTTP as install.ps1 expects:
    # <base>/windows-x86_64/<binary>.
    $site = Join-Path $staging 'site'
    [IO.Directory]::CreateDirectory((Join-Path $site 'windows-x86_64')) | Out-Null
    Copy-Item -Path (Join-Path $ArtifactDirectory '*') -Destination (Join-Path $site 'windows-x86_64')
    $probe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $probe.Start(); $port = $probe.LocalEndpoint.Port; $probe.Stop()
    $python = (Get-Command python.exe -ErrorAction Stop).Source
    $server = Start-Process -FilePath $python -PassThru -WindowStyle Hidden -ArgumentList @(
        '-I', '-m', 'http.server', "$port", '--bind', '127.0.0.1', '--directory', $site)
    $deadline = (Get-Date).AddSeconds(20)
    while ($true) {
        try { Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$port/windows-x86_64/SHA256SUMS" | Out-Null; break } catch {
            if ((Get-Date) -gt $deadline) { throw 'loopback artifact server did not start' }
            Start-Sleep -Milliseconds 200
        }
    }

    $install = Invoke-AsStandardUser -Label 'install.ps1' -Command (
        "& $(Quote $stagedScript) -ArtifactBase 'http://127.0.0.1:$port/'")
    if ($install -notmatch 'Installed Treer') { $failures += 'install.ps1 did not report an installation' }
    $installed = Join-Path $env:SystemDrive "Users\$userName\AppData\Local\Treer\bin"
    foreach ($binary in @('treer', 'treer-launch', 'treer-agent-host', 'treer-agent-server')) {
        if (-not (Test-Path -LiteralPath (Join-Path $installed "$binary.exe"))) { $failures += "$binary.exe was not installed for the standard user" }
    }
    Invoke-AsStandardUser -Label 'service status' -Command (
        "& (Join-Path `$env:LOCALAPPDATA 'Treer\bin\treer-agent-server.exe') service status") | Out-Null
    foreach ($test in $TestExecutable) {
        Invoke-AsStandardUser -Label (Split-Path -Leaf $test) -Command (
            "& $(Quote ([IO.Path]::GetFullPath($test))) --test-threads=1") | Out-Null
    }
    if ($AgentServerTest) {
        $test = Quote ([IO.Path]::GetFullPath($AgentServerTest))
        $lock = Invoke-AsStandardUser -Label 'registration lock' -Command "& $test service::tests::machine_"
        if ($lock -notmatch 'test result: ok\. [1-9]') { $failures += 'registration lock tests did not run' }
        $realHost = Invoke-AsStandardUser -Label 'real Host' -Command "& $test windows_real_host --ignored"
        if ($realHost -notmatch 'test result: ok\. 1 passed') { $failures += 'real Host test did not run' }
    }
} finally {
    if ($server) { Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue }
    Remove-LocalUser -Name $userName -ErrorAction SilentlyContinue
    $profilePath = Join-Path $env:SystemDrive "Users\$userName"
    if (Test-Path $profilePath) { Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $profilePath }
    if (Test-Path $staging) { Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $staging }
}
if ($failures.Count) {
    throw "standard-user check failed: $($failures -join '; ')"
}
Write-Host 'standard-user check passed: install, service status and the native tests succeeded as a standard user.'
