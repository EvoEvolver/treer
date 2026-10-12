param([Parameter(Mandatory=$true)][string]$ArtifactDirectory)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$ArtifactDirectory = [IO.Path]::GetFullPath($ArtifactDirectory)
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('treer-installer-test-' + [Guid]::NewGuid().ToString('N'))
$installation = Join-Path $temporary 'bin with spaces'
$previousUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$previousPath = $env:Path
$script:checksumOverride = $null
# Exercise the actual installer with native release executables. Keep network
# responses deterministic; installation, process checks, hashes and PATH are real.
function Invoke-WebRequest {
    param([switch]$UseBasicParsing, [string]$Uri, [string]$OutFile)
    $binary = ([Uri]$Uri).Segments[-1]
    if ($binary -eq 'SHA256SUMS') {
        $content = Get-Content -LiteralPath (Join-Path $ArtifactDirectory $binary) -Raw
        if ($script:checksumOverride) { $content = $script:checksumOverride }
        return [PSCustomObject]@{ Content = $content }
    }
    Copy-Item -LiteralPath (Join-Path $ArtifactDirectory $binary) -Destination $OutFile
}
try {
    foreach ($scriptFile in @('scripts/install.ps1', 'scripts/stage-release-artifacts.ps1')) {
        $tokens = $null
        $errors = $null
        [Management.Automation.Language.Parser]::ParseFile((Resolve-Path $scriptFile), [ref]$tokens, [ref]$errors) | Out-Null
        if ($errors.Count) { throw ($errors | Out-String) }
    }
    & ./scripts/install.ps1 -ArtifactBase 'https://fixture.invalid/artifacts/' -InstallDir $installation
    & ./scripts/install.ps1 -ArtifactBase 'https://fixture.invalid/artifacts/' -InstallDir $installation
    foreach ($binary in @('treer', 'treer-agent-host', 'treer-agent-server')) {
        if ((Get-Command "$binary.exe").Source -ne (Join-Path $installation "$binary.exe")) { throw "PATH failed for $binary." }
    }
    $entries = ([Environment]::GetEnvironmentVariable('Path', 'User') -split ';') | Where-Object { $_ -eq $installation }
    if (@($entries).Count -ne 1) { throw 'Installer duplicated the PATH entry.' }
    $before = (Get-FileHash (Join-Path $installation 'treer.exe')).Hash
    $remaining = Get-Content (Join-Path $ArtifactDirectory 'SHA256SUMS') | Select-Object -Skip 1
    $script:checksumOverride = ((@(('0' * 64) + '  treer') + @($remaining)) -join "`n") + "`n"
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
