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
            policy
                .validate_command_execution_for_shell(command, false, ShellDialect::PowerShell,)
                .is_err(),
            "PowerShell expression bypass must be rejected: {command:?}"
        );
    }
}

#[test]
fn strict_powershell_grammar_precedes_named_allowlist_exemptions() {
    let policy = SecurityPolicy::default();

    for command in [
        "echo \"$([System.IO.File]::Delete('important.txt'))\"",
        "echo \"$env:NAME\"",
        "echo Env:NAME",
    ] {
        assert_eq!(
            policy.command_risk_level_for_shell(command, ShellDialect::PowerShell),
            CommandRiskLevel::High,
            "unsupported PowerShell syntax must remain high risk: {command:?}"
        );
        let error = policy
            .validate_command_execution_for_shell(command, true, ShellDialect::PowerShell)
            .expect_err("named allowlist entries must not exempt unsupported PowerShell grammar");
        assert!(
            error.contains("not allowed"),
            "unsupported PowerShell grammar must fail at the structural allowlist gate: {error}"
        );
    }
}

#[test]
fn disabled_high_risk_blocking_does_not_relax_named_allowlist_grammar() {
    let policy = SecurityPolicy {
        allowed_commands: vec!["echo".into()],
        block_high_risk_commands: false,
        ..SecurityPolicy::default()
    };

    for command in [
        "echo \"$([System.IO.File]::Delete('important.txt'))\"",
        "echo \"$env:NAME\"",
        "echo Env:NAME",
    ] {
        assert!(
            policy
                .validate_command_execution_for_shell(command, true, ShellDialect::PowerShell)
                .is_err(),
            "only wildcard plus disabled high-risk blocking may opt out of structural guards"
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
        "wsl.exe --exec sh -c 'rm important.txt'",
        "customalias important.txt",
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

#[test]
fn powershell_batch_files_do_not_inherit_native_command_policy() {
    use zeroclaw_config::policy::AutonomyLevel;

    for (allowed, command) in [
        ("git", r".\git.bat status"),
        ("git", r".\git.cmd status"),
        ("git.bat", "git.bat status"),
        (r".\git.cmd", r".\git.cmd status"),
        ("*", r".\git.bat status"),
    ] {
        let policy = SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec![allowed.into()],
            block_high_risk_commands: true,
            ..SecurityPolicy::default()
        };

        assert_eq!(
            policy.command_risk_level_for_shell(command, ShellDialect::PowerShell),
            CommandRiskLevel::High,
            "batch file must not inherit the native command's risk: {command:?}"
        );
        let error = policy
            .validate_command_execution_for_shell(command, true, ShellDialect::PowerShell)
            .expect_err("bounded PowerShell grammar must reject batch files");
        assert!(
            error.contains("not allowed"),
            "batch file must fail at the structural allowlist gate: {error}"
        );
    }
}

#[test]
fn powershell_exe_application_still_matches_native_allowlist_entry() {
    let policy = SecurityPolicy {
        allowed_commands: vec!["git".into()],
        ..SecurityPolicy::default()
    };

    assert_eq!(
        policy
            .validate_command_execution_for_shell(
                "git.exe status",
                false,
                ShellDialect::PowerShell,
            )
            .unwrap(),
        CommandRiskLevel::Low
    );
}

#[test]
fn mutation_aliases_and_scoped_variables_are_high_risk() {
    let policy = SecurityPolicy {
        autonomy: zeroclaw_config::policy::AutonomyLevel::Full,
        allowed_commands: vec!["*".into()],
        block_high_risk_commands: true,
        ..SecurityPolicy::default()
    };

    for command in [
        "ac .\\review-proof.txt value",
        "clc .\\review-proof.txt",
        "Write-Output $env:NAME",
        "Write-Output $global:name",
        "Write-Output $script:name",
    ] {
        assert_eq!(
            policy.command_risk_level_for_shell(command, ShellDialect::PowerShell),
            CommandRiskLevel::High,
            "PowerShell trust-boundary case must be high risk: {command:?}"
        );
        assert!(
            policy
                .validate_command_execution_for_shell(command, true, ShellDialect::PowerShell,)
                .is_err(),
            "wildcard must not exempt high-risk PowerShell syntax: {command:?}"
        );
    }

    assert_eq!(
        policy.command_risk_level_for_shell("Write-Output '$env:NAME'", ShellDialect::PowerShell,),
        CommandRiskLevel::Low,
        "single-quoted text must not be parsed as a scoped variable"
    );
}

#[test]
fn wildcard_and_risk_flags_keep_their_existing_approval_semantics() {
    use zeroclaw_config::policy::AutonomyLevel;

    let supervised = SecurityPolicy {
        autonomy: AutonomyLevel::Supervised,
        allowed_commands: vec!["*".into()],
        block_high_risk_commands: false,
        require_approval_for_medium_risk: true,
        ..SecurityPolicy::default()
    };

    assert_eq!(
        supervised
            .validate_command_execution_for_shell(
                "Write-Output \"quoted safe value\" | Select-Object -First 1",
                false,
                ShellDialect::PowerShell,
            )
            .unwrap(),
        CommandRiskLevel::Low
    );

    for command in ["New-Item output.txt", "Copy-Item from.txt to.txt"] {
        assert!(
            supervised
                .validate_command_execution_for_shell(command, false, ShellDialect::PowerShell,)
                .unwrap_err()
                .contains("requires explicit approval")
        );
        assert_eq!(
            supervised
                .validate_command_execution_for_shell(command, true, ShellDialect::PowerShell,)
                .unwrap(),
            CommandRiskLevel::Medium
        );
    }

    for command in [
        "ac output.txt value",
        "wsl.exe --exec echo unsafe",
        r".\git.bat status",
        "Write-Output $env:NAME",
        "echo \"$([System.IO.File]::Delete('important.txt'))\"",
        "echo Env:NAME",
    ] {
        assert!(
            supervised
                .validate_command_execution_for_shell(command, false, ShellDialect::PowerShell,)
                .unwrap_err()
                .contains("requires explicit approval")
        );
        assert_eq!(
            supervised
                .validate_command_execution_for_shell(command, true, ShellDialect::PowerShell,)
                .unwrap(),
            CommandRiskLevel::High
        );
    }

    let full = SecurityPolicy {
        autonomy: AutonomyLevel::Full,
        ..supervised
    };
    assert_eq!(
        full.validate_command_execution_for_shell(
            "ac output.txt value",
            false,
            ShellDialect::PowerShell,
        )
        .unwrap(),
        CommandRiskLevel::High,
        "full autonomy plus disabled high-risk blocking must remain permissive"
    );
}
