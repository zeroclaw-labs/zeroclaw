param(
    [Parameter(Mandatory = $true)]
    [string]$FixturePath,
    [Parameter(Mandatory = $true)]
    [string]$ConfigDir,
    [Parameter(Mandatory = $true)]
    [string]$EvidenceDir,
    [switch]$CleanupOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$taskName = 'ZeroClaw Daemon'
$maxLogBytes = 8MB
$stdoutMarker = 'ZEROCLAW_STDOUT_界_MARKER'
$stderrMarker = 'ZEROCLAW_STDERR_界_MARKER'
$fixture = if (Test-Path -LiteralPath $FixturePath) {
    (Resolve-Path -LiteralPath $FixturePath).Path
} else {
    [IO.Path]::GetFullPath($FixturePath)
}
$ConfigDir = [IO.Path]::GetFullPath($ConfigDir)
$legacyConfigDir = Join-Path $env:RUNNER_TEMP 'zeroclaw-legacy-service-smoke'
$lookalikeConfigDir = Join-Path $env:RUNNER_TEMP 'zeroclaw-unrelated-service-smoke'
$legacyWrapper = Join-Path $legacyConfigDir 'zeroclaw-daemon.cmd'
$legacyStdout = Join-Path $legacyConfigDir 'daemon.stdout.log'
$legacyStderr = Join-Path $legacyConfigDir 'daemon.stderr.log'
$evidence = [ordered]@{
    tested_sha = (git rev-parse HEAD).Trim()
    runner = $env:RUNNER_NAME
    administrator = $false
    action = $null
    running_state = $null
    running_result = $null
    runner_process_id = $null
    daemon_process_id = $null
    descendant_process_id = $null
    legacy_wrapper_process_id = $null
    legacy_daemon_process_id = $null
    legacy_descendant_process_id = $null
    active_legacy_reinstall_refused = $false
    ready_legacy_reinstall_refused = $false
    active_legacy_registration_preserved = $false
    disabled_legacy_migration_succeeded = $false
    legacy_process_tree_stopped_before_reinstall = $false
    legacy_lookalike_survived_reinstall = $false
    direct_lookalike_survived_reinstall = $false
    stdout_bytes = $null
    stderr_bytes = $null
    capture_setup_failure_result = $null
    limitations = @(
        'The hosted runner is elevated, so this does not reproduce non-elevated installation failure.'
        'The task is started manually, so this does not prove the ONLOGON trigger.'
    )
}
$legacyLookalike = $null
$directLookalike = $null

function Invoke-Fixture {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Arguments)
    $output = & $fixture --config-dir $ConfigDir @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "Fixture failed ($LASTEXITCODE): $($output -join [Environment]::NewLine)"
    }
    return ($output -join [Environment]::NewLine)
}

function Wait-Until {
    param(
        [Parameter(Mandatory = $true)][scriptblock]$Condition,
        [Parameter(Mandatory = $true)][string]$Description,
        [int]$TimeoutSeconds = 45
    )
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for $Description"
}

function Set-CurrentUserOwner {
    param([Parameter(Mandatory = $true)][string]$Path)
    $acl = Get-Acl -LiteralPath $Path
    $acl.SetOwner([Security.Principal.WindowsIdentity]::GetCurrent().User)
    Set-Acl -LiteralPath $Path -AclObject $acl
}

function Remove-SmokeTask {
    $descendantPids = @()
    foreach ($pidFile in @(
        (Join-Path $ConfigDir 'descendant.pid'),
        (Join-Path $legacyConfigDir 'descendant.pid')
    )) {
        if (Test-Path -LiteralPath $pidFile) {
            $descendantPids += [int](Get-Content -LiteralPath $pidFile -Raw).Trim()
        }
    }
    if (Test-Path -LiteralPath $fixture) {
        & $fixture --config-dir $ConfigDir service stop *> $null
        & $fixture --config-dir $ConfigDir service uninstall *> $null
    }
    schtasks /Delete /TN $taskName /F *> $null
    Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object {
            $_.ExecutablePath -eq $fixture -and
            ($_.CommandLine -like "*$ConfigDir*" -or $_.CommandLine -like "*$legacyConfigDir*")
        } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    foreach ($descendantPid in $descendantPids) {
        $descendant = Get-CimInstance Win32_Process -Filter "ProcessId = $descendantPid" -ErrorAction SilentlyContinue
        if ($null -ne $descendant -and
            $descendant.Name -eq 'powershell.exe' -and
            $descendant.CommandLine -like '*Start-Sleep -Seconds 600*') {
            Stop-Process -Id $descendantPid -Force -ErrorAction SilentlyContinue
            Wait-Until -Description 'fallback descendant cleanup' -TimeoutSeconds 10 -Condition {
                $null -eq (Get-Process -Id $descendantPid -ErrorAction SilentlyContinue)
            }
        }
    }
}

if ($CleanupOnly) {
    Remove-SmokeTask
    Remove-Item -LiteralPath $ConfigDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $legacyConfigDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $lookalikeConfigDir -Recurse -Force -ErrorAction SilentlyContinue
    exit 0
}

$transcript = Join-Path $EvidenceDir 'windows-service-smoke-transcript.txt'
$transcriptStarted = $false
$stdoutLog = Join-Path $ConfigDir 'logs\daemon.stdout.log'
$stderrLog = Join-Path $ConfigDir 'logs\daemon.stderr.log'
$descendantPidFile = Join-Path $ConfigDir 'descendant.pid'
$runnerError = Join-Path $ConfigDir 'runner-error.txt'

try {
    New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
    Start-Transcript -Path $transcript -Force | Out-Null
    $transcriptStarted = $true
    Remove-SmokeTask
    Remove-Item -LiteralPath $ConfigDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $legacyConfigDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $lookalikeConfigDir -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null
    New-Item -ItemType Directory -Force -Path $legacyConfigDir | Out-Null
    Set-CurrentUserOwner -Path $ConfigDir
    New-Item -ItemType Directory -Force -Path (Join-Path $ConfigDir 'logs') | Out-Null
    Set-CurrentUserOwner -Path (Join-Path $ConfigDir 'logs')
    foreach ($logPath in @($stdoutLog, $stderrLog)) {
        New-Item -ItemType File -Force -Path $logPath | Out-Null
        Set-CurrentUserOwner -Path $logPath
    }

    $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    $evidence.administrator = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if (-not $evidence.administrator) { throw 'Hosted Windows smoke requires an elevated runner' }

    @(
        '@echo off'
        ('"{0}" --config-dir "{1}" daemon >>"{2}" 2>>"{3}"' -f $fixture, $legacyConfigDir, $legacyStdout, $legacyStderr)
    ) | Set-Content -LiteralPath $legacyWrapper -Encoding Ascii
    & schtasks /Create /TN $taskName /SC ONLOGON /TR $legacyWrapper /RL LIMITED /F | Write-Host
    if ($LASTEXITCODE -ne 0) { throw "Failed to register legacy task: $LASTEXITCODE" }
    & schtasks /Run /TN $taskName | Write-Host
    if ($LASTEXITCODE -ne 0) { throw "Failed to start legacy task: $LASTEXITCODE" }
    Wait-Until -Description 'legacy wrapper, daemon, and descendant startup' -TimeoutSeconds 90 -Condition {
        $legacyWrapperProcess = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -eq 'cmd.exe' -and $_.CommandLine -like "*$legacyWrapper*" } |
            Select-Object -First 1
        (Test-Path -LiteralPath (Join-Path $legacyConfigDir 'daemon-started.pid')) -and
            (Test-Path -LiteralPath (Join-Path $legacyConfigDir 'descendant.pid')) -and
            ($null -ne $legacyWrapperProcess)
    }
    $legacyWrapperProcess = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -eq 'cmd.exe' -and $_.CommandLine -like "*$legacyWrapper*" } |
        Select-Object -First 1
    $legacyDaemonPid = [int](Get-Content -LiteralPath (Join-Path $legacyConfigDir 'daemon-started.pid') -Raw).Trim()
    $legacyDescendantPid = [int](Get-Content -LiteralPath (Join-Path $legacyConfigDir 'descendant.pid') -Raw).Trim()
    $evidence.legacy_wrapper_process_id = $legacyWrapperProcess.ProcessId
    $evidence.legacy_daemon_process_id = $legacyDaemonPid
    $evidence.legacy_descendant_process_id = $legacyDescendantPid

    $legacyLookalike = Start-Process -FilePath $env:ComSpec -ArgumentList @('/K', ('type "{0}"' -f $legacyWrapper)) -WindowStyle Hidden -PassThru
    Wait-Until -Description 'unrelated shell mentioning the wrapper' -Condition {
        $process = Get-CimInstance Win32_Process -Filter "ProcessId = $($legacyLookalike.Id)" -ErrorAction SilentlyContinue
        $null -ne $process -and $process.CommandLine -like "*$legacyWrapper*"
    }

    $legacyInstallOutput = & $fixture --config-dir $ConfigDir service install 2>&1
    $legacyInstallExit = $LASTEXITCODE
    $legacyInstallOutput | Write-Host
    if ($legacyInstallExit -eq 0 -or $legacyInstallOutput -notmatch 'Cannot safely replace the legacy Windows \.cmd task') {
        throw "Active legacy reinstall did not fail with the expected guidance: exit=$legacyInstallExit"
    }
    $evidence.active_legacy_reinstall_refused = $true
    $legacyTask = Get-ScheduledTask -TaskName $taskName
    $legacyAction = $legacyTask.Actions | Select-Object -First 1
    $legacyExecute = [string]$legacyAction.Execute
    if (-not $legacyExecute.Trim('"').EndsWith('.cmd', [System.StringComparison]::OrdinalIgnoreCase)) {
        throw 'Active legacy reinstall replaced the existing task registration'
    }
    $evidence.active_legacy_registration_preserved = $true
    $legacyProcessesPreserved =
        ($null -ne (Get-Process -Id $legacyWrapperProcess.ProcessId -ErrorAction SilentlyContinue)) -and
        ($null -ne (Get-Process -Id $legacyDaemonPid -ErrorAction SilentlyContinue)) -and
        ($null -ne (Get-Process -Id $legacyDescendantPid -ErrorAction SilentlyContinue))
    if (-not $legacyProcessesPreserved) { throw 'Active legacy reinstall terminated a process without proven ownership' }
    if ($null -eq (Get-Process -Id $legacyLookalike.Id -ErrorAction SilentlyContinue)) {
        throw 'Reinstall killed an unrelated shell that only mentioned the legacy wrapper'
    }
    $evidence.legacy_lookalike_survived_reinstall = $true

    & taskkill.exe /PID $legacyWrapperProcess.ProcessId /T /F *> $null
    if ($LASTEXITCODE -ne 0) { throw 'Failed to clean up the test-owned legacy process tree' }
    Wait-Until -Description 'test-owned legacy process tree cleanup' -Condition {
        ($null -eq (Get-Process -Id $legacyWrapperProcess.ProcessId -ErrorAction SilentlyContinue)) -and
            ($null -eq (Get-Process -Id $legacyDaemonPid -ErrorAction SilentlyContinue)) -and
            ($null -eq (Get-Process -Id $legacyDescendantPid -ErrorAction SilentlyContinue)) -and
            ([int](Get-ScheduledTask -TaskName $taskName).State -ne 4) -and
            ([int](Get-ScheduledTask -TaskName $taskName).State -ne 2)
    }
    $evidence.legacy_process_tree_stopped_before_reinstall = $true

    $readyInstallOutput = & $fixture --config-dir $ConfigDir service install 2>&1
    $readyInstallExit = $LASTEXITCODE
    $readyInstallOutput | Write-Host
    if ($readyInstallExit -eq 0 -or $readyInstallOutput -notmatch 'Cannot safely replace the legacy Windows \.cmd task') {
        throw "Ready legacy reinstall did not fail with the expected guidance: exit=$readyInstallExit"
    }
    $evidence.ready_legacy_reinstall_refused = $true
    Disable-ScheduledTask -TaskName $taskName | Out-Null
    Wait-Until -Description 'disabled legacy task before migration' -Condition {
        [int](Get-ScheduledTask -TaskName $taskName).State -eq 1
    }
    Invoke-Fixture service install | Write-Host
    $evidence.disabled_legacy_migration_succeeded = $true
    $task = Get-ScheduledTask -TaskName $taskName
    $action = $task.Actions | Select-Object -First 1
    $evidence.action = "$($action.Execute) $($action.Arguments)"
    $actionExecutable = $action.Execute.Trim('"')
    if ($actionExecutable -ne $fixture) { throw "Task action executable mismatch: $($action.Execute)" }
    if ($action.Arguments -notlike "*service run-windows-daemon*") { throw 'Task action does not use the production Windows service runner' }
    if ($action.Arguments -notlike "*$ConfigDir*") { throw 'Task action omitted the isolated config directory' }

    $startedAt = [DateTime]::UtcNow
    Invoke-Fixture service start | Write-Host
    Wait-Until -Description 'both bounded Unicode markers' -TimeoutSeconds 90 -Condition {
        $taskState = Get-ScheduledTask -TaskName $taskName
        $taskInfo = Get-ScheduledTaskInfo -TaskName $taskName
        if ([int]$taskState.State -ne 4 -and
            $taskInfo.LastRunTime.ToUniversalTime() -ge $startedAt.AddSeconds(-2) -and
            $taskInfo.LastTaskResult -ne 267009 -and
            $taskInfo.LastTaskResult -ne 267011) {
            throw "Windows service task exited before markers: state=$($taskState.State) result=$($taskInfo.LastTaskResult)"
        }
        if (-not (Test-Path -LiteralPath $stdoutLog) -or
            -not (Test-Path -LiteralPath $stderrLog)) {
            return $false
        }
        try {
            # Each marker is written only after its stream emits more than 8 MiB.
            # The bounded pending queue may shed older burst data, so retained
            # file size is not a valid lower bound for bytes captured.
            return ((Get-Content -LiteralPath $stdoutLog -Encoding UTF8 -Tail 4) -contains $stdoutMarker) -and
                ((Get-Content -LiteralPath $stderrLog -Encoding UTF8 -Tail 4) -contains $stderrMarker)
        } catch {
            return $false
        }
    }

    $task = Get-ScheduledTask -TaskName $taskName
    $taskInfo = Get-ScheduledTaskInfo -TaskName $taskName
    $evidence.running_state = [string]$task.State
    $evidence.running_result = $taskInfo.LastTaskResult
    if ([int]$task.State -ne 4) { throw "Task is not running: $($task.State)" }
    $statusOutput = Invoke-Fixture service status
    $statusOutput | Write-Host
    if ($statusOutput -match 'not running' -or $statusOutput -notmatch 'Service:.*running') { throw "Service status did not report running: $statusOutput" }

    $processes = Get-CimInstance Win32_Process | Where-Object {
        $_.ExecutablePath -eq $fixture -and $_.CommandLine -like "*$ConfigDir*"
    }
    $runnerProcess = $processes | Where-Object { $_.CommandLine -like '*service run-windows-daemon*' } | Select-Object -First 1
    $daemonProcess = $processes | Where-Object { $_.CommandLine -like '* daemon*' } | Select-Object -First 1
    if ($null -eq $runnerProcess -or $null -eq $daemonProcess) { throw 'Expected runner and daemon fixture processes were not found' }
    $evidence.runner_process_id = $runnerProcess.ProcessId
    $evidence.daemon_process_id = $daemonProcess.ProcessId

    $stdoutInfo = Get-Item -LiteralPath $stdoutLog
    $stderrInfo = Get-Item -LiteralPath $stderrLog
    $evidence.stdout_bytes = $stdoutInfo.Length
    $evidence.stderr_bytes = $stderrInfo.Length
    if ($stdoutInfo.Length -gt $maxLogBytes -or $stderrInfo.Length -gt $maxLogBytes) { throw 'A capture file exceeded the 8 MiB bound' }
    if ($stdoutInfo.LastWriteTimeUtc -lt $startedAt -or $stderrInfo.LastWriteTimeUtc -lt $startedAt) { throw 'Capture files are not fresh for this run' }
    $logsEvidence = Join-Path $EvidenceDir 'service-logs.txt'
    & $fixture --config-dir $ConfigDir service logs *> $logsEvidence
    if ($LASTEXITCODE -ne 0) { throw "service logs failed with exit code $LASTEXITCODE" }
    $stdoutVisible = Get-Content -LiteralPath $logsEvidence -Encoding UTF8 | Select-String -Pattern $stdoutMarker -SimpleMatch -Quiet
    $stderrVisible = Get-Content -LiteralPath $logsEvidence -Encoding UTF8 | Select-String -Pattern $stderrMarker -SimpleMatch -Quiet
    if (-not $stdoutVisible -or -not $stderrVisible) { throw 'service logs did not render both Unicode markers' }

    $descendantPid = [int](Get-Content -LiteralPath $descendantPidFile -Raw).Trim()
    $evidence.descendant_process_id = $descendantPid
    $descendantProcess = Get-CimInstance Win32_Process -Filter "ProcessId = $descendantPid"
    if ($null -eq $descendantProcess) { throw 'Fixture descendant was not running before stop' }
    if ($daemonProcess.ParentProcessId -ne $runnerProcess.ProcessId) { throw 'Daemon is not a direct child of the service runner' }
    if ($descendantProcess.ParentProcessId -ne $daemonProcess.ProcessId) { throw 'Fixture descendant is not a direct child of the daemon' }
    Invoke-Fixture service stop | Write-Host
    Wait-Until -Description 'task and descendant shutdown' -Condition {
        ([int](Get-ScheduledTask -TaskName $taskName).State -ne 4) -and
        ($null -eq (Get-Process -Id $runnerProcess.ProcessId -ErrorAction SilentlyContinue)) -and
        ($null -eq (Get-Process -Id $daemonProcess.ProcessId -ErrorAction SilentlyContinue)) -and
        ($null -eq (Get-Process -Id $descendantPid -ErrorAction SilentlyContinue))
    }

    New-Item -ItemType Directory -Force -Path $lookalikeConfigDir | Out-Null
    Set-CurrentUserOwner -Path $lookalikeConfigDir
    New-Item -ItemType Directory -Force -Path (Join-Path $lookalikeConfigDir 'logs') | Out-Null
    Set-CurrentUserOwner -Path (Join-Path $lookalikeConfigDir 'logs')
    foreach ($logName in @('daemon.stdout.log', 'daemon.stderr.log')) {
        New-Item -ItemType File -Force -Path (Join-Path $lookalikeConfigDir 'logs' $logName) | Out-Null
        Set-CurrentUserOwner -Path (Join-Path $lookalikeConfigDir 'logs' $logName)
    }
    $directLookalike = Start-Process -FilePath $fixture -ArgumentList @('--config-dir', ('"{0}"' -f $lookalikeConfigDir), 'service', 'run-windows-daemon') -WindowStyle Hidden -PassThru
    Wait-Until -Description 'unrelated same-binary runner' -TimeoutSeconds 90 -Condition {
        (Test-Path -LiteralPath (Join-Path $lookalikeConfigDir 'daemon-started.pid')) -and
            ($null -ne (Get-Process -Id $directLookalike.Id -ErrorAction SilentlyContinue))
    }
    Remove-Item -LiteralPath (Join-Path $ConfigDir 'daemon-started.pid'), (Join-Path $ConfigDir 'descendant.pid') -Force -ErrorAction SilentlyContinue
    Invoke-Fixture service start | Write-Host
    Wait-Until -Description 'registered direct runner tree before reinstall' -Condition {
        ([int](Get-ScheduledTask -TaskName $taskName).State -eq 4) -and
            (Test-Path -LiteralPath (Join-Path $ConfigDir 'daemon-started.pid')) -and
            (Test-Path -LiteralPath (Join-Path $ConfigDir 'descendant.pid'))
    }
    $registeredRunner = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object {
            $_.ExecutablePath -eq $fixture -and
            $_.CommandLine -like "*$ConfigDir*" -and
            $_.CommandLine -like '*service run-windows-daemon*'
        } | Select-Object -First 1
    if ($null -eq $registeredRunner) { throw 'Registered direct runner was not found before reinstall' }
    $registeredDaemonPid = [int](Get-Content -LiteralPath (Join-Path $ConfigDir 'daemon-started.pid') -Raw).Trim()
    $registeredDescendantPid = [int](Get-Content -LiteralPath (Join-Path $ConfigDir 'descendant.pid') -Raw).Trim()
    $registeredDaemon = Get-CimInstance Win32_Process -Filter "ProcessId = $registeredDaemonPid"
    $registeredDescendant = Get-CimInstance Win32_Process -Filter "ProcessId = $registeredDescendantPid"
    if ($null -eq $registeredDaemon -or $registeredDaemon.ParentProcessId -ne $registeredRunner.ProcessId -or
        $null -eq $registeredDescendant -or $registeredDescendant.ParentProcessId -ne $registeredDaemonPid) {
        throw 'Registered direct runner tree did not have the expected process ancestry'
    }
    Invoke-Fixture service install | Write-Host
    if (($null -ne (Get-Process -Id $registeredRunner.ProcessId -ErrorAction SilentlyContinue)) -or
        ($null -ne (Get-Process -Id $registeredDaemonPid -ErrorAction SilentlyContinue)) -or
        ($null -ne (Get-Process -Id $registeredDescendantPid -ErrorAction SilentlyContinue))) {
        throw 'Registered direct runner tree survived service reinstall'
    }
    if ($null -eq (Get-Process -Id $directLookalike.Id -ErrorAction SilentlyContinue)) {
        throw 'Reinstall killed an unrelated same-binary runner with another config directory'
    }
    $evidence.direct_lookalike_survived_reinstall = $true

    Invoke-Fixture service uninstall | Write-Host
    Remove-Item -LiteralPath (Join-Path $ConfigDir 'logs') -Recurse -Force
    Set-Content -LiteralPath (Join-Path $ConfigDir 'logs') -Value 'blocks log directory creation' -NoNewline
    Invoke-Fixture service install | Write-Host
    Remove-Item -LiteralPath (Join-Path $ConfigDir 'daemon-started.pid') -Force -ErrorAction SilentlyContinue
    $beforeFailure = Get-ScheduledTaskInfo -TaskName $taskName
    $failureStartedAt = [DateTime]::UtcNow
    Invoke-Fixture service start | Write-Host
    Wait-Until -Description 'nonzero task result from capture setup failure' -Condition {
        $info = Get-ScheduledTaskInfo -TaskName $taskName
        ([int](Get-ScheduledTask -TaskName $taskName).State -ne 4) -and
        ($info.LastRunTime -gt $beforeFailure.LastRunTime) -and
        ($info.LastRunTime.ToUniversalTime() -ge $failureStartedAt.AddSeconds(-2)) -and
        ($info.LastTaskResult -ne 0) -and
        ($info.LastTaskResult -ne 267009) -and
        ($info.LastTaskResult -ne 267011)
    }
    $evidence.capture_setup_failure_result = (Get-ScheduledTaskInfo -TaskName $taskName).LastTaskResult
    if (Test-Path -LiteralPath (Join-Path $ConfigDir 'daemon-started.pid')) { throw 'Daemon started despite capture setup failure' }

    $evidence | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $EvidenceDir 'windows-service-smoke.json') -Encoding UTF8
}
finally {
    foreach ($ownedProcess in @($legacyLookalike, $directLookalike)) {
        if ($null -ne $ownedProcess -and $null -ne (Get-Process -Id $ownedProcess.Id -ErrorAction SilentlyContinue)) {
            & taskkill.exe /PID $ownedProcess.Id /T /F *> $null
        }
    }
    try {
        $taskState = Get-ScheduledTask -TaskName $taskName
        $taskInfo = Get-ScheduledTaskInfo -TaskName $taskName
        [ordered]@{
            state = [string]$taskState.State
            last_run_time_utc = $taskInfo.LastRunTime.ToUniversalTime().ToString('O')
            last_task_result = $taskInfo.LastTaskResult
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $EvidenceDir 'service-task.json') -Encoding UTF8
    } catch {
        Write-Warning "Task evidence capture failed: $_"
    }
    foreach ($logPath in @($stdoutLog, $stderrLog)) {
        if (Test-Path -LiteralPath $logPath) {
            $logName = Split-Path -Leaf $logPath
            Get-Content -LiteralPath $logPath -Encoding UTF8 -Tail 4 |
                Set-Content -LiteralPath (Join-Path $EvidenceDir "$logName.tail.txt") -Encoding UTF8
            [ordered]@{
                bytes = (Get-Item -LiteralPath $logPath).Length
                last_write_time_utc = (Get-Item -LiteralPath $logPath).LastWriteTimeUtc.ToString('O')
            } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $EvidenceDir "$logName.json") -Encoding UTF8
        }
    }
    if (Test-Path -LiteralPath $runnerError) {
        Copy-Item -LiteralPath $runnerError -Destination (Join-Path $EvidenceDir 'runner-error.txt') -Force
    }
    try { Remove-SmokeTask } catch { Write-Warning "Cleanup failed: $_" }
    Remove-Item -LiteralPath $ConfigDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $legacyConfigDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $lookalikeConfigDir -Recurse -Force -ErrorAction SilentlyContinue
    if ($transcriptStarted) { Stop-Transcript | Out-Null }
}

exit 0
