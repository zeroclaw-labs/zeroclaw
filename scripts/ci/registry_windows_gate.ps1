# Focused native Windows checks for registry install commands.
# Usage: powershell.exe -NoProfile -File scripts/ci/registry_windows_gate.ps1
# Prerequisites: Rust 1.98.0, MSVC build tools, Git, cmd.exe, Windows PowerShell,
# and the x86_64-pc-windows-msvc and wasm32-wasip2 targets for that toolchain.
# Builds a fresh argument recorder, then runs the focused regression tests.
# No tools are installed. The temporary build directory is retained.
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:OS -ne 'Windows_NT') { throw 'Native Windows is required; no skip path.' }
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$target = 'x86_64-pc-windows-msvc'
# Check installation before selecting it: rustup must not auto-install a missing
# pinned toolchain as a side effect of invoking Cargo.
$installed = @(& rustup toolchain list)
if ($LASTEXITCODE -ne 0 -or -not ($installed -match '^1\.98\.0(?:-|\s)')) {
    throw 'Preinstall toolchain 1.98.0 on the authorized host; this gate installs nothing.'
}
$targets = @(& rustup target list --installed --toolchain 1.98.0)
if ($LASTEXITCODE -ne 0 -or -not ($targets -contains 'wasm32-wasip2') -or -not ($targets -contains $target)) {
    throw 'Preinstall x86_64-pc-windows-msvc and wasm32-wasip2 for toolchain 1.98.0.'
}
Get-Command cargo, git, cmd.exe, powershell.exe -ErrorAction Stop | Out-Null
# A fresh directory prevents the recorder and the root Quickstart WASM fixture
# from resolving to artifacts built for an older candidate. Keep for evidence.
$targetDir = Join-Path ([IO.Path]::GetTempPath()) ('zeroclaw-registry-gate-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($targetDir) | Out-Null
$oldTargetDir = $env:CARGO_TARGET_DIR
$oldFixture = $env:ZEROCLAW_REGISTRY_ARGV_FIXTURE
$oldToolchain = $env:RUSTUP_TOOLCHAIN
$env:CARGO_TARGET_DIR = $targetDir
# Nested Cargo in the existing Quickstart fixture must use the same toolchain.
$env:RUSTUP_TOOLCHAIN = '1.98.0'

function Invoke-CandidateTest {
    param([string[]]$CargoArgs, [int]$ExpectedPassed = 1)
    Write-Host ('cargo ' + ($CargoArgs -join ' '))
    $lines = @(& cargo @CargoArgs)
    $code = $LASTEXITCODE
    $lines | ForEach-Object { Write-Host $_ }
    if ($code -ne 0) { throw "Candidate test failed with exit $code" }
    $text = $lines -join [Environment]::NewLine
    $expected = 'test result: ok. ' + $ExpectedPassed + ' passed; 0 failed;'
    if (-not $text.Contains($expected)) { throw "Expected executed test count missing: $expected" }
}

Push-Location $repo
try {
    & git rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Cannot identify candidate HEAD.' }
    & git diff --stat
    if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect candidate diff.' }
    Write-Host "Fresh candidate build directory: $targetDir"
    # Cargo JSON identifies the actual executable produced by THIS successful
    # build. Never guess an old target/examples path or accept caller fixture.
    $messages = @(& cargo build --locked -p zeroclaw-plugins --example registry_argv_fixture --target $target --message-format=json)
    if ($LASTEXITCODE -ne 0) { throw 'Candidate recorder build failed.' }
    $executables = @($messages | ForEach-Object {
        $message = $_ | ConvertFrom-Json
        if ($message.reason -eq 'compiler-artifact' -and
            $message.target.name -eq 'registry_argv_fixture' -and
            $null -ne $message.executable) { $message.executable }
    })
    if ($executables.Count -ne 1) { throw 'Expected exactly one candidate recorder executable.' }
    $fixture = (Resolve-Path -LiteralPath $executables[0]).Path
    $prefix = [IO.Path]::GetFullPath($targetDir).TrimEnd('\') + '\'
    if (-not $fixture.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Cargo recorder artifact is outside this fresh candidate directory.'
    }
    $env:ZEROCLAW_REGISTRY_ARGV_FIXTURE = $fixture
    Write-Host "Candidate recorder: $fixture"
    Get-FileHash -Algorithm SHA256 -LiteralPath $fixture | Format-List | Out-Host

    Invoke-CandidateTest -ExpectedPassed 3 -CargoArgs @('test', '--locked', '-p', 'zeroclaw-plugins', '--target', $target, '--test', 'registry_windows_commands')
    Invoke-CandidateTest -CargoArgs @('test', '--locked', '-p', 'zeroclaw-plugins', '--target', $target, '--test', 'registry_literal_compat', 'baseline_registry_literals_remain_source_compatible', '--', '--exact')
    Invoke-CandidateTest -CargoArgs @('test', '--locked', '-p', 'zeroclaw-plugins', '--target', $target, '--lib', 'registry::tests::raw_registry_arguments_render_for_each_supported_shell', '--', '--exact')
    Invoke-CandidateTest -CargoArgs @('test', '--locked', '-p', 'zeroclaw-plugins', '--target', $target, '--lib', 'registry::tests::registry_copy_ready_boundary_refuses_unsafe_windows_values_only', '--', '--exact')
    Invoke-CandidateTest -CargoArgs @('test', '--locked', '-p', 'zeroclaw', '--target', $target, '--features', 'plugins-wasm-cranelift', '--test', 'component', 'component::plugin_registry_windows_cli::windows_plugin_list_reports_unavailable_recipe_without_hiding_metadata', '--', '--exact')
    Invoke-CandidateTest -CargoArgs @('test', '--locked', '-p', 'zeroclaw-runtime', '--target', $target, '--features', 'plugins-wasm-cranelift', '--lib', 'skills::suggestions::tests::windows_cached_plugin_suggestion_keeps_metadata_but_has_no_unsafe_recipe', '--', '--exact')
    # Existing root helper builds current in-tree tool-fixture with --locked,
    # --target wasm32-wasip2 in this fresh target's tmp/plugin-load-check-fixture.
    # It fails on missing target/compiler; no prebuilt WASM or skip is accepted.
    Invoke-CandidateTest -CargoArgs @('test', '--locked', '-p', 'zeroclaw', '--target', $target, '--features', 'plugins-wasm-cranelift', '--bin', 'zeroclaw', 'quickstart_plugins::tests::windows_load_failure_retains_failure_without_unsafe_registry_retry', '--', '--exact')
    Write-Host 'Native registry gate completed; inspect all recorded output before qualification.'
} finally {
    Pop-Location
    $env:CARGO_TARGET_DIR = $oldTargetDir
    $env:ZEROCLAW_REGISTRY_ARGV_FIXTURE = $oldFixture
    $env:RUSTUP_TOOLCHAIN = $oldToolchain
}
