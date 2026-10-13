param([Parameter(Mandatory=$true)][string]$ArtifactDirectory)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$ArtifactDirectory = [IO.Path]::GetFullPath($ArtifactDirectory)
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('treer-installer-test-' + [Guid]::NewGuid().ToString('N'))
$installation = Join-Path $temporary 'bin with spaces'
$previousUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$previousPath = $env:Path
$fixture = @{ ArtifactDirectory = $ArtifactDirectory; ChecksumOverride = $null; RequestCount = 0 }
# Exercise the actual installer with native release executables. Keep network
# responses deterministic; installation, process checks, hashes and PATH are real.
${function:Invoke-WebRequest} = {
    param([switch]$UseBasicParsing, [string]$Uri, [string]$OutFile)
    $fixture.RequestCount += 1
    $binary = ([Uri]$Uri).Segments[-1]
    if ($binary -eq 'SHA256SUMS') {
        $content = Get-Content -LiteralPath (Join-Path $fixture.ArtifactDirectory $binary) -Raw
        if ($fixture.ChecksumOverride) { $content = $fixture.ChecksumOverride }
        return [PSCustomObject]@{ Content = $content }
    }
    Copy-Item -LiteralPath (Join-Path $fixture.ArtifactDirectory $binary) -Destination $OutFile
}.GetNewClosure()
try {
    foreach ($scriptFile in @('scripts/install.ps1', 'scripts/stage-release-artifacts.ps1')) {
        $tokens = $null
        $errors = $null
        [Management.Automation.Language.Parser]::ParseFile((Resolve-Path $scriptFile), [ref]$tokens, [ref]$errors) | Out-Null
        if ($errors.Count) { throw ($errors | Out-String) }
    }
    # Rejected origins must fail before any request or installation write.
    $rejectedCaseNumber = 0
    foreach ($case in @(
        @{ Uri = 'http://fixture.invalid/artifacts/'; Error = '*Refusing cleartext HTTP artifacts*' }
        @{ Uri = 'http://127.0.0.1.fixture.invalid/artifacts/'; Error = '*Refusing cleartext HTTP artifacts*' }
        @{ Uri = 'http://localhost.fixture.invalid/artifacts/'; Error = '*Refusing cleartext HTTP artifacts*' }
        @{ Uri = 'http://[::2]/artifacts/'; Error = '*Refusing cleartext HTTP artifacts*' }
        @{ Uri = 'http://127.0.0.2/artifacts/'; Error = '*Refusing cleartext HTTP artifacts*' }
        @{ Uri = 'http://[::1%251]/artifacts/'; Error = '*Refusing cleartext HTTP artifacts*' }
        @{ Uri = 'http://[%3A%3A1]/artifacts/'; Error = '*supply an HTTP(S) -ArtifactBase URL*' }
        @{ Uri = 'http://[::ffff:127.0.0.1]/artifacts/'; Error = '*Refusing cleartext HTTP artifacts*' }
        @{ Uri = 'https://fixture-user:fixture-password@fixture.invalid/artifacts/'; Error = '*must not contain credentials*' }
        @{ Uri = 'http://fixture-user:fixture-password@127.0.0.1/artifacts/'; Error = '*must not contain credentials*' }
    )) {
        $rejectedCaseNumber += 1
        Write-Host ('Testing rejected artifact origin case ' + $rejectedCaseNumber)
        $beforeRequests = $fixture.RequestCount
        $rejected = $false
        try {
            & ./scripts/install.ps1 -ArtifactBase $case.Uri -InstallDir $installation
        } catch {
            if ($_.Exception.Message -notlike $case.Error) { throw }
            $rejected = $true
        }
        if (!$rejected -or $fixture.RequestCount -ne $beforeRequests -or (Test-Path -LiteralPath $installation)) {
            throw 'Rejected artifact origin reached the download or installation step.'
        }
    }
    # Network responses stay mocked, but URL parsing and the actual install are real.
    $allowedCaseNumber = 0
    foreach ($origin in @(
        'https://fixture.invalid/artifacts/'
        'http://localhost/artifacts/'
        'http://LOCALHOST/artifacts/'
        'http://127.0.0.1/artifacts/'
        'http://[::1]/artifacts/'
        'http://[0:0:0:0:0:0:0:1]/artifacts/'
    )) {
        $allowedCaseNumber += 1
        Write-Host ('Testing allowed artifact origin case ' + $allowedCaseNumber)
        $beforeRequests = $fixture.RequestCount
        & ./scripts/install.ps1 -ArtifactBase $origin -InstallDir $installation
        if ($fixture.RequestCount -le $beforeRequests) { throw 'Allowed artifact origin did not reach the download step.' }
    }
    & ./scripts/install.ps1 -ArtifactBase 'https://fixture.invalid/artifacts/' -InstallDir $installation
    foreach ($binary in @('treer', 'treer-agent-host', 'treer-agent-server')) {
        if ((Get-Command "$binary.exe").Source -ne (Join-Path $installation "$binary.exe")) { throw "PATH failed for $binary." }
    }
    $entries = ([Environment]::GetEnvironmentVariable('Path', 'User') -split ';') | Where-Object { $_ -eq $installation }
    if (@($entries).Count -ne 1) { throw 'Installer duplicated the PATH entry.' }
    $before = (Get-FileHash (Join-Path $installation 'treer.exe')).Hash
    $remaining = Get-Content (Join-Path $ArtifactDirectory 'SHA256SUMS') | Select-Object -Skip 1
    $fixture.ChecksumOverride = ((@(('0' * 64) + '  treer') + @($remaining)) -join "`n") + "`n"
    $rejected = $false
    try {
        & ./scripts/install.ps1 -ArtifactBase 'https://fixture.invalid/artifacts/' -InstallDir $installation
    } catch {
        if ($_.Exception.Message -notlike '*Checksum mismatch*') { throw }
        $rejected = $true
    }
    if (!$rejected -or (Get-FileHash (Join-Path $installation 'treer.exe')).Hash -ne $before) { throw 'Failed download changed an existing installation.' }
    Write-Host 'Windows installer smoke tests passed.'
} finally {
    [Environment]::SetEnvironmentVariable('Path', $previousUserPath, 'User')
    $env:Path = $previousPath
    if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Recurse -Force }
}
