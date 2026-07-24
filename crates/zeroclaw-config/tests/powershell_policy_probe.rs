use zeroclaw_api::runtime_traits::ShellDialect;
use zeroclaw_config::policy::{CommandRiskLevel, SecurityPolicy};

fn powershell_policy() -> SecurityPolicy {
    let mut policy = SecurityPolicy::default();
    policy
        .allowed_commands
        .extend(["write-output", "get-date", "get-childitem", "get-location"].map(str::to_string));
    policy
}

#[test]
fn powershell_expressions_hidden_behind_allowed_commands_fail_closed() {
    let policy = powershell_policy();

    for command in [
        "echo ([System.IO.File]::Delete('important.txt'))",
        "Write-Output $(Remove-Item important.txt)",
        "Write-Output safe; Remove-Item important.txt",
        "Write-Output safe | Invoke-Expression",
        "Write-Output & $command",
        "Write-Output { Remove-Item important.txt }",
        "Write-Output \"safe\\\"; Remove-Item important.txt",
        "Get-ChildItem $PSHOME",
        "Get-ChildItem Env:",
        "Write-Output $PSHOME | Get-ChildItem",
    ] {
        assert_eq!(
            policy.command_risk_level_for_shell(command, ShellDialect::PowerShell),
            CommandRiskLevel::High,
            "unsupported PowerShell syntax must be high risk: {command:?}"
        );
        assert!(
            !policy.is_command_allowed_for_shell(command, ShellDialect::PowerShell),
            "unsupported PowerShell syntax must fail closed: {command:?}"
        );
        assert!(
            policy
                .validate_command_execution_for_shell(command, false, ShellDialect::PowerShell,)
                .is_err(),
            "PowerShell expression bypass must be rejected: {command:?}"
        );
    }
}

#[test]
fn documented_read_only_powershell_commands_pass_default_risk_gates() {
    let policy = powershell_policy();

    for command in [
        "Write-Output safe",
        "Get-Date",
        "Get-ChildItem",
        "Get-Location",
        "Write-Output $PSHOME",
        "Write-Output $PSVersionTable.PSVersion",
        "Get-ChildItem | Write-Output",
    ] {
        assert_eq!(
            policy
                .validate_command_execution_for_shell(command, false, ShellDialect::PowerShell,)
                .unwrap_or_else(|error| panic!("{command:?} was rejected: {error}")),
            CommandRiskLevel::Low,
            "read-only PowerShell command should stay low risk: {command:?}"
        );
    }
}

#[test]
fn unknown_powershell_cmdlets_are_high_risk_by_default() {
    let policy = SecurityPolicy {
        allowed_commands: vec!["*".into()],
        ..SecurityPolicy::default()
    };

    assert_eq!(
        policy.command_risk_level_for_shell("Add-Type custom.cs", ShellDialect::PowerShell),
        CommandRiskLevel::High
    );
    assert!(
        policy
            .validate_command_execution_for_shell(
                "Add-Type custom.cs",
                true,
                ShellDialect::PowerShell,
            )
            .is_err()
    );

    for command in [
        ".\\evil.ps1",
        "powershell.exe -Command Get-Date",
        "cmd.exe /C dir",
    ] {
        assert_eq!(
            policy.command_risk_level_for_shell(command, ShellDialect::PowerShell),
            CommandRiskLevel::High,
            "nested interpreters and scripts must be high risk: {command:?}"
        );
        assert!(
            policy
                .validate_command_execution_for_shell(command, true, ShellDialect::PowerShell,)
                .is_err(),
            "nested interpreter or script must be blocked: {command:?}"
        );
    }
}
