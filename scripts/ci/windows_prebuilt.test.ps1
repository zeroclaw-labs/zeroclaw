# Run with Windows PowerShell 5.1 or PowerShell 7. Network calls use local fixtures.
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$installer = Join-Path $repo 'scripts/install_windows_prebuilt.ps1'
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('zeroclaw-checksum-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixture | Out-Null
$fixtureState = @{}
$originalTemp = $env:TEMP
$originalTmp = $env:TMP
$originalProfile = $env:USERPROFILE

function Assert($Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Invoke-RestMethod {
    param([string]$Uri)
    Assert ($Uri -eq 'https://api.github.com/repos/zeroclaw-labs/zeroclaw/releases/latest') 'Unexpected release request'
    $fixtureState.releaseCalls++
    # A second resolution sees a different release, so mixed-tag downloads fail.
    @{ tag_name = @('v1.2.3', 'v9.9.9')[[Math]::Min($fixtureState.releaseCalls - 1, 1)] }
}

function Invoke-WebRequest {
    param([string]$Uri, [string]$OutFile, [switch]$UseBasicParsing)
    $fixtureState.urls += $Uri
    $prefix = 'https://github.com/zeroclaw-labs/zeroclaw/releases/download/v1.2.3/'
    Assert ($Uri.StartsWith($prefix, [StringComparison]::Ordinal)) 'Download did not use the resolved release'
    switch ($Uri.Substring($prefix.Length)) {
        'SHA256SUMS' {
            if ($fixtureState.scenario -eq 'missing-manifest') { throw 'Fixture: manifest unavailable' }
            [IO.File]::WriteAllText($OutFile, $fixtureState.manifest)
        }
        'zeroclaw-x86_64-pc-windows-msvc.zip' {
            if ($fixtureState.scenario -eq 'partial-download') {
                [IO.File]::WriteAllText($OutFile, 'partial')
                throw 'Fixture: download interrupted'
            }
            Copy-Item -LiteralPath $fixtureState.archive -Destination $OutFile
        }
        default { throw 'Unexpected release asset' }
    }
}

try {
    $env:TEMP = $fixture
    $env:TMP = $fixture
    $env:USERPROFILE = Join-Path $fixture 'profile with spaces'
    $payload = Join-Path $fixture 'payload.txt'
    [IO.File]::WriteAllText($payload, 'new release payload')
    $fixtureState.archive = Join-Path $fixture 'fixture.zip'
    Compress-Archive -LiteralPath $payload -DestinationPath $fixtureState.archive
    $digest = (Get-FileHash -LiteralPath $fixtureState.archive -Algorithm SHA256).Hash.ToLowerInvariant()
    $asset = 'zeroclaw-x86_64-pc-windows-msvc.zip'
    $valid = "$digest  $asset"
    $guide = [IO.File]::ReadAllText((Join-Path $repo 'docs/book/src/setup/windows.md'))
    $block = [regex]::Match($guide, '(?s)```powershell\r?\n(.*?)\r?\n```').Groups[1].Value
    Assert ($block.Length -gt 0) 'Documented installer block is missing'
    $document = [scriptblock]::Create($block)
    $helper = [IO.File]::ReadAllText($installer).TrimEnd() -replace '\r\n', "`n"
    $embedded = [regex]::Match($block, '(?s)    & \{\r?\n(.*?)\r?\n    \} -ReleaseTag').Groups[1].Value
    $embedded = ($embedded -replace '(?m)^        ', '') -replace '\r\n', "`n"
    Assert ($helper -ceq $embedded) 'Guide must embed the production helper verbatim'

    $cases = @(
        @{ Name = 'text'; Manifest = $valid; Success = $true },
        @{ Name = 'binary-marker'; Manifest = "$digest *$asset"; Success = $true },
        @{ Name = 'uppercase-hash'; Manifest = "$($digest.ToUpperInvariant())  $asset"; Success = $true },
        @{ Name = 'other-assets'; Manifest = "$('0' * 64)  another.zip`n$valid"; Success = $true },
        @{ Name = 'corrupt'; Manifest = "$('0' * 64)  $asset"; Error = 'SHA256 mismatch' },
        @{ Name = 'missing-entry'; Manifest = "$digest  another.zip"; Error = 'exactly one' },
        @{ Name = 'substring'; Manifest = "$digest  $asset.bak"; Error = 'exactly one' },
        @{ Name = 'case-mismatch'; Manifest = "$digest  $($asset.ToUpperInvariant())"; Error = 'exactly one' },
        @{ Name = 'duplicate'; Manifest = "$valid`n$valid"; Error = 'exactly one' },
        @{ Name = 'duplicate-malformed'; Manifest = "$valid`ninvalid  $asset"; Error = 'exactly one' },
        @{ Name = 'malformed'; Manifest = "invalid  $asset"; Error = 'Invalid SHA256SUMS' },
        @{ Name = 'missing-manifest'; Manifest = ''; Error = 'manifest unavailable' },
        @{ Name = 'partial-download'; Manifest = $valid; Error = 'download interrupted' }
    )
    $count = 0
    foreach ($case in $cases) {
        # Full documented failure paths must stop before persistent PATH and Quickstart.
        $surfaces = @('helper')
        if (-not $case.Success) { $surfaces += 'document' }
        foreach ($surface in $surfaces) {
            $fixtureState.scenario = $case.Name
            $fixtureState.manifest = $case.Manifest
            $fixtureState.urls = @()
            $fixtureState.releaseCalls = 0
            $env:USERPROFILE = Join-Path $fixture ("$surface $($case.Name)")
            $destination = Join-Path $env:USERPROFILE '.zeroclaw/bin'
            New-Item -ItemType Directory -Path $destination -Force | Out-Null
            $existing = Join-Path $destination 'payload.txt'
            [IO.File]::WriteAllText($existing, 'existing install')
            $caught = $null
            try {
                if ($surface -eq 'helper') { & $installer -DestinationPath $destination }
                else { & $document }
            } catch { $caught = $_.Exception.Message }
            if ($case.Success) {
                Assert ($null -eq $caught) "$surface/$($case.Name): $caught"
                Assert ([IO.File]::ReadAllText($existing) -eq 'new release payload') 'Verified archive was not installed'
            } else {
                Assert ($null -ne $caught -and $caught.Contains($case.Error)) "$surface/$($case.Name): unexpected error: $caught"
                Assert ([IO.File]::ReadAllText($existing) -eq 'existing install') 'Failed check modified the install'
                Assert (@(Get-ChildItem -LiteralPath $destination).Count -eq 1) 'Failed check extracted an extra file'
            }
            Assert ($fixtureState.releaseCalls -eq 1) 'Latest release must be resolved once'
            Assert (@(Get-ChildItem -LiteralPath $fixture -Filter 'zeroclaw-install-*').Count -eq 0) 'Temporary download leaked'
            $count++
            Write-Host "PASS $surface/$($case.Name)"
        }
    }
    $fixtureState.releaseCalls = 0
    $fixtureState.urls = @()
    $fixtureState.scenario = 'text'
    $fixtureState.manifest = $valid
    & $installer -ReleaseTag 'v1.2.3' -DestinationPath (Join-Path $fixture 'explicit tag')
    Assert ($fixtureState.releaseCalls -eq 0) 'Explicit tag unexpectedly resolved latest'
    foreach ($tag in @('../latest', 'v1.2.3/other', 'not-a-version')) {
        $fixtureState.urls = @()
        $caught = $null
        try { & $installer -ReleaseTag $tag -DestinationPath (Join-Path $fixture 'invalid tag') }
        catch { $caught = $_.Exception.Message }
        Assert ($caught -like '*valid version tag*') 'Invalid tag did not fail closed'
        Assert ($fixtureState.urls.Count -eq 0) 'Invalid tag initiated an asset download'
    }
    $batchRoot = Join-Path $fixture 'batch entrypoint'
    $batchScripts = Join-Path $batchRoot 'scripts'
    $stubs = Join-Path $batchRoot 'stubs'
    New-Item -ItemType Directory -Path $batchScripts, $stubs | Out-Null
    Copy-Item -LiteralPath (Join-Path $repo 'setup.bat') -Destination $batchRoot
    $wrapper = @'
$ErrorActionPreference = 'Stop'
function Invoke-RestMethod { @{ tag_name = 'v1.2.3' } }
function Invoke-WebRequest {
    param($Uri, $OutFile, [switch]$UseBasicParsing)
    if ($Uri.EndsWith('/SHA256SUMS')) {
        [IO.File]::WriteAllText($OutFile, ('0' * 64) + '  zeroclaw-x86_64-pc-windows-msvc.zip')
    } else { [IO.File]::WriteAllText($OutFile, 'corrupt archive') }
}
& $env:ZEROCLAW_TEST_INSTALLER
'@
    [IO.File]::WriteAllText((Join-Path $batchScripts 'install_windows_prebuilt.ps1'), $wrapper)
    foreach ($command in @('cargo', 'curl', 'setx', 'tar', 'rustup')) {
        [IO.File]::WriteAllText((Join-Path $stubs "$command.cmd"), "@echo off`r`necho UNEXPECTED_$command`r`nexit /b 91`r`n")
    }
    $batchProfile = Join-Path $batchRoot 'profile'
    $batchBin = Join-Path $batchProfile '.zeroclaw/bin'
    New-Item -ItemType Directory -Path $batchBin -Force | Out-Null
    $batchExisting = Join-Path $batchBin 'existing.txt'
    [IO.File]::WriteAllText($batchExisting, 'existing install')
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.FileName = $env:ComSpec
    $start.Arguments = '/d /c setup.bat --prebuilt'
    $start.WorkingDirectory = $batchRoot
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.EnvironmentVariables['USERPROFILE'] = $batchProfile
    $start.EnvironmentVariables['PATH'] = "$stubs;$env:PATH"
    $start.EnvironmentVariables['ZEROCLAW_TEST_INSTALLER'] = $installer
    $start.EnvironmentVariables.Remove('PSModulePath')
    $process = [Diagnostics.Process]::Start($start)
    $outputTask = $process.StandardOutput.ReadToEndAsync()
    $errorTask = $process.StandardError.ReadToEndAsync()
    Assert ($process.WaitForExit(30000)) 'Batch entrypoint timed out'
    $output = $outputTask.GetAwaiter().GetResult() + $errorTask.GetAwaiter().GetResult()
    Assert ($process.ExitCode -eq 1) "Batch must return failure: $output"
    Assert ($output.Contains('SHA256 mismatch') -and $output.Contains('Setup failed.')) "Batch did not propagate the checksum failure: $output"
    Assert (-not $output.Contains('UNEXPECTED_')) "Batch attempted a fallback or side effect: $output"
    Assert ([IO.File]::ReadAllText($batchExisting) -eq 'existing install') 'Batch changed the existing install'
    Assert (@(Get-ChildItem -LiteralPath $batchBin).Count -eq 1) 'Batch extracted an unverified archive'
    $process.Dispose()
    Write-Host 'PASS setup.bat --prebuilt: checksum mismatch, exit 1, no extraction, source fallback or PATH write'
    Write-Host "PASS $count checksum cases, explicit tag and 3 invalid tags; all network calls mocked"
} finally {
    $env:TEMP = $originalTemp
    $env:TMP = $originalTmp
    $env:USERPROFILE = $originalProfile
    Remove-Item -LiteralPath $fixture -Recurse -Force
}
