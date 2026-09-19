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
    stdout_bytes = $null
    stderr_bytes = $null
    capture_setup_failure_result = $null
    limitations = @(
        'The hosted runner is elevated, so this does not reproduce non-elevated installation failure.'
        'The task is started manually, so this does not prove the ONLOGON trigger.'
    )
}

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
    $descendantPid = if (Test-Path -LiteralPath (Join-Path $ConfigDir 'descendant.pid')) {
        [int](Get-Content -LiteralPath (Join-Path $ConfigDir 'descendant.pid') -Raw).Trim()
    } else {
        $null
    }
    if (Test-Path -LiteralPath $fixture) {
        & $fixture --config-dir $ConfigDir service stop *> $null
        & $fixture --config-dir $ConfigDir service uninstall *> $null
    }
    schtasks /Delete /TN $taskName /F *> $null
    Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object { $_.ExecutablePath -eq $fixture -and $_.CommandLine -like "*$ConfigDir*" } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    if ($null -ne $descendantPid) {
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
    New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null
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

    Invoke-Fixture service install | Write-Host
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
    if ($transcriptStarted) { Stop-Transcript | Out-Null }
}

exit 0
