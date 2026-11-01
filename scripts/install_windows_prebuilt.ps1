param(
    [string]$ReleaseTag,
    [string]$DestinationPath = (Join-Path $env:USERPROFILE '.zeroclaw/bin')
)

$ErrorActionPreference = 'Stop'
if (-not $ReleaseTag) {
    $ReleaseTag = (Invoke-RestMethod -Uri 'https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/latest').tag_name
}
if ($ReleaseTag -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$') {
    throw 'The release response did not contain a valid version tag.'
}

$asset = 'zeroclaw-x86_64-pc-windows-msvc.zip'
$releaseUrl = "https://github.com/zeroclaw-labs/zeroclaw/releases/download/$ReleaseTag"
$workDir = Join-Path ([IO.Path]::GetTempPath()) ('zeroclaw-install-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $workDir | Out-Null
try {
    $archive = Join-Path $workDir $asset
    $manifest = Join-Path $workDir 'SHA256SUMS'
    Invoke-WebRequest -Uri "$releaseUrl/SHA256SUMS" -OutFile $manifest -UseBasicParsing
    Invoke-WebRequest -Uri "$releaseUrl/$asset" -OutFile $archive -UseBasicParsing

    $entrySuffix = '[ *]' + [regex]::Escape($asset) + '$'
    $entries = @(Get-Content -LiteralPath $manifest | Where-Object { $_ -cmatch $entrySuffix })
    if ($entries.Count -ne 1) {
        throw "SHA256SUMS must contain exactly one checksum for $asset."
    }
    if ($entries[0] -cnotmatch ('^[0-9a-fA-F]{64} ' + $entrySuffix)) {
        throw "Invalid SHA256SUMS entry for $asset."
    }
    $expected = $entries[0].Substring(0, 64)
    $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
    if ($actual -ne $expected) {
        throw "SHA256 mismatch for $asset. The archive was not extracted."
    }

    Write-Host "SHA256 verified: $asset ($ReleaseTag)"
    Expand-Archive -LiteralPath $archive -DestinationPath $DestinationPath -Force
} finally {
    Remove-Item -LiteralPath $workDir -Recurse -Force
}
