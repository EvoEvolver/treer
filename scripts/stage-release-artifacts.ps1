param([Parameter(Mandatory=$true)][string]$Destination)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') { throw 'Requires a native Windows x64 runner.' }
$commit = $env:TREER_BUILD_COMMIT
if (!$commit) { $commit = git rev-parse HEAD }
if ($commit -notmatch '^[0-9a-fA-F]{7,64}$') { throw 'Invalid build commit.' }
[IO.Directory]::CreateDirectory($Destination) | Out-Null
$version = $null
$checksums = @()
foreach ($binary in @('treer', 'treer-agent-host', 'treer-agent-server')) {
    $source = "target\release\$binary.exe"
    $identity = & $source --version
    if ($LASTEXITCODE -ne 0 -or $identity -notmatch "^\S+ (\S+) \($commit\)$") { throw "Invalid build identity for $binary." }
    $currentVersion = $Matches[1]
    if ($version -and $version -ne $currentVersion) { throw 'Binary versions do not match.' }
    $version = $currentVersion
    # Public artifact keys retain their logical names on every platform.
    # The Windows installer gives the downloaded files their .exe suffix.
    $destinationFile = Join-Path $Destination $binary
    Copy-Item -LiteralPath $source -Destination $destinationFile
    $checksums += (Get-FileHash -Algorithm SHA256 $destinationFile).Hash.ToLowerInvariant() + "  $binary"
}
$utf8 = New-Object System.Text.UTF8Encoding($false)
[IO.File]::WriteAllText((Join-Path $Destination 'SHA256SUMS'), (($checksums -join "`n") + "`n"), $utf8)
$metadata = @{ schema_version = 1; git_commit = $commit; version = $version; platform = 'windows-x86_64'; rustc = (rustc --version) }
[IO.File]::WriteAllText((Join-Path $Destination 'build-metadata.json'), ($metadata | ConvertTo-Json), $utf8)
