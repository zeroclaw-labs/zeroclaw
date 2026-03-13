use super::*;
use clap::{CommandFactory, Parser};

#[test]
fn cli_definition_has_no_flag_conflicts() {
    Cli::command().debug_assert();
}

#[test]
fn onboard_help_includes_model_flag() {
    let cmd = Cli::command();
    let onboard = cmd
        .get_subcommands()
        .find(|subcommand| subcommand.get_name() == "onboard")
        .expect("onboard subcommand must exist");

    let has_model_flag = onboard
        .get_arguments()
        .any(|arg| arg.get_id().as_str() == "model" && arg.get_long() == Some("model"));

    assert!(
        has_model_flag,
        "onboard help should include --model for quick setup overrides"
    );
}

#[test]
fn onboard_cli_accepts_model_provider_and_api_key_in_quick_mode() {
    let cli = Cli::try_parse_from([
        "zeroclaw",
        "onboard",
        "--provider",
        "openrouter",
        "--model",
        "custom-model-946",
        "--api-key",
        "sk-issue946",
    ])
    .expect("quick onboard invocation should parse");

    match cli.command {
        Commands::Onboard {
            interactive,
            force,
            channels_only,
            api_key,
            provider,
            model,
            ..
        } => {
            assert!(!interactive);
            assert!(!force);
            assert!(!channels_only);
            assert_eq!(provider.as_deref(), Some("openrouter"));
            assert_eq!(model.as_deref(), Some("custom-model-946"));
            assert_eq!(api_key.as_deref(), Some("sk-issue946"));
        }
        other => panic!("expected onboard command, got {other:?}"),
    }
}

#[test]
fn completions_cli_parses_supported_shells() {
    for shell in ["bash", "fish", "zsh", "powershell", "elvish"] {
        let cli = Cli::try_parse_from(["zeroclaw", "completions", shell])
            .expect("completions invocation should parse");
        match cli.command {
            Commands::Completions { .. } => {}
            other => panic!("expected completions command, got {other:?}"),
        }
    }
}

#[test]
fn completion_generation_mentions_binary_name() {
    let mut output = Vec::new();
    write_shell_completion(CompletionShell::Bash, &mut output)
        .expect("completion generation should succeed");
    let script = String::from_utf8(output).expect("completion output should be valid utf-8");
    assert!(
        script.contains("zeroclaw"),
        "completion script should reference binary name"
    );
}

#[test]
fn onboard_cli_accepts_force_flag() {
    let cli = Cli::try_parse_from(["zeroclaw", "onboard", "--force"])
        .expect("onboard --force should parse");

    match cli.command {
        Commands::Onboard { force, .. } => assert!(force),
        other => panic!("expected onboard command, got {other:?}"),
    }
}

#[test]
fn cli_parses_estop_default_engage() {
    let cli = Cli::try_parse_from(["zeroclaw", "estop"]).expect("estop command should parse");

    match cli.command {
        Commands::Estop {
            estop_command,
            level,
            domains,
            tools,
        } => {
            assert!(estop_command.is_none());
            assert!(level.is_none());
            assert!(domains.is_empty());
            assert!(tools.is_empty());
        }
        other => panic!("expected estop command, got {other:?}"),
    }
}

#[test]
fn cli_parses_estop_resume_domain() {
    let cli = Cli::try_parse_from(["zeroclaw", "estop", "resume", "--domain", "*.chase.com"])
        .expect("estop resume command should parse");

    match cli.command {
        Commands::Estop {
            estop_command: Some(EstopSubcommands::Resume { domains, .. }),
            ..
        } => assert_eq!(domains, vec!["*.chase.com".to_string()]),
        other => panic!("expected estop resume command, got {other:?}"),
    }
}

#[test]
fn agent_command_parses_with_temperature() {
    let cli = Cli::try_parse_from(["zeroclaw", "agent", "--temperature", "0.5"])
        .expect("agent command with temperature should parse");

    match cli.command {
        Commands::Agent { temperature, .. } => {
            assert_eq!(temperature, Some(0.5));
        }
        other => panic!("expected agent command, got {other:?}"),
    }
}

#[test]
fn agent_command_parses_without_temperature() {
    let cli = Cli::try_parse_from(["zeroclaw", "agent", "--message", "hello"])
        .expect("agent command without temperature should parse");

    match cli.command {
        Commands::Agent { temperature, .. } => {
            assert_eq!(temperature, None);
        }
        other => panic!("expected agent command, got {other:?}"),
    }
}

#[test]
fn agent_fallback_uses_config_default_temperature() {
    // Test that when user doesn't provide --temperature,
    // the fallback logic works correctly
    let mut config = Config::default(); // default_temperature = 0.7
    config.default_temperature = 1.5;

    // Simulate None temperature (user didn't provide --temperature)
    let user_temperature: Option<f64> = std::hint::black_box(None);
    let final_temperature = user_temperature.unwrap_or(config.default_temperature);

    assert!((final_temperature - 1.5).abs() < f64::EPSILON);
}

#[test]
fn agent_fallback_uses_hardcoded_when_config_uses_default() {
    // Test that when config uses default value (0.7), fallback still works
    let config = Config::default(); // default_temperature = 0.7

    // Simulate None temperature (user didn't provide --temperature)
    let user_temperature: Option<f64> = std::hint::black_box(None);
    let final_temperature = user_temperature.unwrap_or(config.default_temperature);

    assert!((final_temperature - 0.7).abs() < f64::EPSILON);
}
