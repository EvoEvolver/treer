<#
.SYNOPSIS
Native check for the Windows administrator product rule (#130), for a
DISPOSABLE GitHub-hosted runner only.

Creates a temporary standard (non-administrator) local user, runs the
installer and the Controller as that user, and asserts that the installer and
`connect` refuse with the fixed code windows_requires_administrator and a
non-zero exit, that `service status` prints the same code, and that nothing
was written under that user's %LOCALAPPDATA%\Treer. The user, its profile and
the staging directory are removed afterwards.

Guards: refuses to run unless TREER_DISPOSABLE_RUNNER=1 AND GITHUB_ACTIONS=true
AND RUNNER_ENVIRONMENT=github-hosted (self-hosted runners such as win_native
report "self-hosted"), so it can never create accounts on a real machine.

.PARAMETER InstallScript  Path to install.ps1 under test.
.PARAMETER AgentServer    Path to the built treer-agent-server.exe under test.
.PARAMETER ArtifactBase   Loopback HTTP(S) artifact base handed to the installer
                          (never reached: the installer must refuse first).
#>
param(
    [Parameter(Mandatory)][string]$InstallScript,
    [Parameter(Mandatory)][string]$AgentServer,
    [string]$ArtifactBase = 'http://127.0.0.1:9/'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:TREER_DISPOSABLE_RUNNER -ne '1' -or $env:GITHUB_ACTIONS -ne 'true' -or
    $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Refusing: this check creates a local user and runs only on a disposable GitHub-hosted runner (TREER_DISPOSABLE_RUNNER=1, GITHUB_ACTIONS=true, RUNNER_ENVIRONMENT=github-hosted).'
}
$code = 'windows_requires_administrator'
$usersSid = 'S-1-5-32-545'
# Secondary Logon is what Start-Process -Credential relies on; say so plainly
# instead of failing later with an opaque error.
$secondaryLogon = Get-Service -Name seclogon -ErrorAction SilentlyContinue
if (-not $secondaryLogon -or $secondaryLogon.Status -ne 'Running') {
    try { Start-Service -Name seclogon -ErrorAction Stop } catch {
        throw 'The Secondary Logon service (seclogon) is not running; Start-Process -Credential cannot run the standard-user checks.'
    }
}
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
function Invoke-AsStandardUser {
    param([string]$FilePath, [string[]]$ArgumentList, [string]$Label, [bool]$RequireNonZeroExit)
    $stdout = [IO.Path]::GetTempFileName()
    $stderr = [IO.Path]::GetTempFileName()
    try {
        # Windows PowerShell 5.1 rejects -NoNewWindow together with -Credential.
        $process = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -Credential $credential `
            -LoadUserProfile -WorkingDirectory $staging -WindowStyle Hidden -Wait -PassThru `
            -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        $output = (Get-Content -Raw $stdout -ErrorAction SilentlyContinue) + (Get-Content -Raw $stderr -ErrorAction SilentlyContinue)
        $exit = $process.ExitCode
    } finally {
        Remove-Item -Force -ErrorAction SilentlyContinue $stdout, $stderr
    }
    $hasCode = [bool]($output -match [regex]::Escape($code))
    Write-Host "[$Label] exit=$exit fixed_code=$hasCode"
    if ($RequireNonZeroExit -and $exit -eq 0) { $script:failures += "$Label exited 0" }
    if (-not $hasCode) { $script:failures += "$Label output lacks $code" }
    # Never echo the standard user's output verbatim beyond the fixed-code check.
}
New-LocalUser -Name $userName -Password $password -AccountNeverExpires -PasswordNeverExpires `
    -Description 'Treer #130 disposable standard-user check' | Out-Null
try {
    Add-LocalGroupMember -Group 'Users' -Member $userName
    # The runner workspace is not readable by a new user: stage copies with
    # read+execute for Users only.
    [IO.Directory]::CreateDirectory($staging) | Out-Null
    & icacls.exe $staging /inheritance:r /grant:r "*${usersSid}:(OI)(CI)RX" /grant:r '*S-1-5-32-544:(OI)(CI)F' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'failed to restrict the staging directory' }
    $stagedScript = Join-Path $staging 'install.ps1'
    $stagedServer = Join-Path $staging 'treer-agent-server.exe'
    Copy-Item -LiteralPath $InstallScript -Destination $stagedScript
    Copy-Item -LiteralPath $AgentServer -Destination $stagedServer
    $profilePath = Join-Path $env:SystemDrive "Users\$userName"
    $treerLocal = Join-Path $profilePath 'AppData\Local\Treer'
    $powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    Invoke-AsStandardUser -Label 'install.ps1' -RequireNonZeroExit $true -FilePath $powershell -ArgumentList @(
        '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $stagedScript, '-ArtifactBase', $ArtifactBase)
    # `service status` stays usable for every account and only prints the code.
    Invoke-AsStandardUser -Label 'service status' -RequireNonZeroExit $false -FilePath $stagedServer -ArgumentList @('service', 'status')
    Invoke-AsStandardUser -Label 'connect' -RequireNonZeroExit $true -FilePath $stagedServer -ArgumentList @(
        'connect', '--key', 'enr_v1_standard_user_check', '--proxy', 'http://127.0.0.1:9/')
    # -LoadUserProfile created the profile, so this assertion is meaningful.
    $profileExists = Test-Path $profilePath
    Write-Host "[profile] exists=$profileExists treer_dir=$(Test-Path $treerLocal)"
    if (-not $profileExists) { $failures += 'standard user profile was not created; the no-write assertion would be vacuous' }
    if (Test-Path $treerLocal) { $failures += "standard user profile contains $treerLocal" }
} finally {
    Remove-LocalUser -Name $userName -ErrorAction SilentlyContinue
    $profilePath = Join-Path $env:SystemDrive "Users\$userName"
    if (Test-Path $profilePath) { Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $profilePath }
    if (Test-Path $staging) { Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $staging }
}
if ($failures.Count) {
    throw "standard-user check failed: $($failures -join '; ')"
}
Write-Host "standard-user check passed: installer and connect refused with $code and non-zero exit, service status printed $code, nothing written to the standard user's %LOCALAPPDATA%\Treer."
