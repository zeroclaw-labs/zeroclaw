#[allow(unused_imports)]
pub use zeroclaw_runtime::sop::*;

use anyhow::Result;

pub fn handle_command(command: crate::SopCommands, config: &crate::config::Config) -> Result<()> {
    let workspace_dir = &config.workspace_dir;
    let default_mode = parse_execution_mode(&config.sop.default_execution_mode);
    let sops = load_sops(workspace_dir, config.sop.sops_dir.as_deref(), default_mode);

    match command {
        crate::SopCommands::List => {
            if sops.is_empty() {
                println!("No SOPs found.");
                println!();
                println!("  Create one: mkdir -p <workspace>/sops/my-sop");
                println!("              then add SOP.toml and SOP.md");
            } else {
                println!("Loaded SOPs ({}):", sops.len());
                println!();
                for sop in &sops {
                    println!(
                        "  {} v{} [{}] — {}",
                        console::style(&sop.name).white().bold(),
                        sop.version,
                        sop.priority,
                        sop.description,
                    );
                    println!(
                        "    Mode: {}  Steps: {}  Triggers: {}",
                        sop.execution_mode,
                        sop.steps.len(),
                        sop.triggers
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
            }
            println!();
            Ok(())
        }
        crate::SopCommands::Validate { name } => {
            let targets: Vec<_> = match &name {
                Some(n) => sops.iter().filter(|s| s.name == *n).collect(),
                None => sops.iter().collect(),
            };

            if targets.is_empty() {
                if let Some(n) = &name {
                    anyhow::bail!("SOP not found: {n}");
                }
                println!("No SOPs found to validate.");
                return Ok(());
            }

            let mut any_warnings = false;
            for sop in &targets {
                let warnings = validate_sop(sop);
                if warnings.is_empty() {
                    println!("  ✅ {} — valid", sop.name);
                } else {
                    any_warnings = true;
                    println!("  ⚠️  {} — {} warning(s):", sop.name, warnings.len());
                    for w in &warnings {
                        println!("       - {w}");
                    }
                }
            }
            if !any_warnings {
                println!();
                println!("All SOPs passed validation.");
            }
            Ok(())
        }
        crate::SopCommands::Show { name } => {
            let sop = sops
                .iter()
                .find(|s| s.name == name)
                .ok_or_else(|| anyhow::anyhow!("SOP not found: {name}"))?;

            println!(
                "{} v{}",
                console::style(&sop.name).white().bold(),
                sop.version
            );
            println!("  {}", sop.description);
            println!();
            println!("  Priority:       {}", sop.priority);
            println!("  Execution mode: {}", sop.execution_mode);
            println!("  Deterministic:  {}", sop.deterministic);
            println!("  Cooldown:       {}s", sop.cooldown_secs);
            println!("  Max concurrent: {}", sop.max_concurrent);
            if let Some(loc) = &sop.location {
                println!("  Location:       {}", loc.display());
            }
            println!();
            println!("  Triggers:");
            for trigger in &sop.triggers {
                println!("    - {trigger}");
            }

            if !sop.steps.is_empty() {
                println!();
                println!("  Steps:");
                for step in &sop.steps {
                    let confirm = if step.requires_confirmation {
                        " [confirmation required]"
                    } else {
                        ""
                    };
                    println!(
                        "    {}. {}{}",
                        step.number,
                        console::style(&step.title).bold(),
                        confirm,
                    );
                    if !step.body.is_empty() {
                        println!("       {}", step.body);
                    }
                    if !step.suggested_tools.is_empty() {
                        println!("       Tools: {}", step.suggested_tools.join(", "));
                    }
                }
            }
            println!();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::types::SopManifest;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn parse_steps_basic() {
        let md = r#"# Test SOP

## Conditions
Some conditions here.

## Steps

1. **Check readings** — Read sensor data and confirm.
   - tools: gpio_read, memory_store

2. **Close valve** — Set GPIO pin 5 LOW.
   - tools: gpio_write, gpio_read
   - requires_confirmation: true

3. **Notify operator** — Send alert.
   - tools: pushover
"#;

        let steps = parse_steps(md);
        assert_eq!(steps.len(), 3);

        assert_eq!(steps[0].number, 1);
        assert_eq!(steps[0].title, "Check readings");
        assert!(steps[0].body.contains("Read sensor data"));
        assert_eq!(steps[0].suggested_tools, vec!["gpio_read", "memory_store"]);
        assert!(!steps[0].requires_confirmation);

        assert_eq!(steps[1].number, 2);
        assert_eq!(steps[1].title, "Close valve");
        assert!(steps[1].requires_confirmation);
        assert_eq!(steps[1].suggested_tools, vec!["gpio_write", "gpio_read"]);

        assert_eq!(steps[2].number, 3);
        assert_eq!(steps[2].title, "Notify operator");
    }

    #[test]
    fn parse_steps_empty_md() {
        let steps = parse_steps("# Nothing here\n\nNo steps section.");
        assert!(steps.is_empty());
    }

    #[test]
    fn parse_steps_no_bold_title() {
        let md = "## Steps\n\n1. Just a plain step without bold.\n";
        let steps = parse_steps(md);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].title, "Just a plain step without bold.");
    }

    #[test]
    fn parse_steps_multiline_body() {
        let md = r#"## Steps

1. **Do thing** — First line of body.
   Second line of body.
   Third line of body.
   - tools: shell
"#;
        let steps = parse_steps(md);
        assert_eq!(steps.len(), 1);
        assert!(steps[0].body.contains("First line"));
        assert!(steps[0].body.contains("Second line"));
        assert!(steps[0].body.contains("Third line"));
    }

    #[test]
    fn load_sop_from_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sop_dir = dir.path().join("test-sop");
        fs::create_dir_all(&sop_dir).unwrap();

        fs::write(
            sop_dir.join("SOP.toml"),
            r#"
[sop]
name = "test-sop"
description = "A test SOP"
version = "1.0.0"
priority = "high"
execution_mode = "auto"
cooldown_secs = 60

[[triggers]]
type = "manual"

[[triggers]]
type = "webhook"
path = "/sop/test"
"#,
        )
        .unwrap();

        fs::write(
            sop_dir.join("SOP.md"),
            r#"# Test SOP

## Steps

1. **Step one** — Do something.
   - tools: shell

2. **Step two** — Do something else.
   - requires_confirmation: true
"#,
        )
        .unwrap();

        let sops = load_sops_from_directory(dir.path(), SopExecutionMode::Supervised);
        assert_eq!(sops.len(), 1);

        let sop = &sops[0];
        assert_eq!(sop.name, "test-sop");
        assert_eq!(sop.priority, SopPriority::High);
        assert_eq!(sop.execution_mode, SopExecutionMode::Auto);
        assert_eq!(sop.cooldown_secs, 60);
        assert_eq!(sop.triggers.len(), 2);
        assert_eq!(sop.steps.len(), 2);
        assert!(sop.steps[1].requires_confirmation);
        assert!(sop.location.is_some());
    }

    #[test]
    fn load_sops_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let sops = load_sops_from_directory(dir.path(), SopExecutionMode::Supervised);
        assert!(sops.is_empty());
    }

    #[test]
    fn load_sops_nonexistent_dir() {
        let sops =
            load_sops_from_directory(Path::new("/nonexistent/path"), SopExecutionMode::Supervised);
        assert!(sops.is_empty());
    }

    #[test]
    fn load_sop_toml_only_no_md() {
        let dir = tempfile::tempdir().unwrap();
        let sop_dir = dir.path().join("no-steps");
        fs::create_dir_all(&sop_dir).unwrap();

        fs::write(
            sop_dir.join("SOP.toml"),
            r#"
[sop]
name = "no-steps"
description = "SOP without steps"

[[triggers]]
type = "manual"
"#,
        )
        .unwrap();

        let sops = load_sops_from_directory(dir.path(), SopExecutionMode::Supervised);
        assert_eq!(sops.len(), 1);
        assert!(sops[0].steps.is_empty());
    }

    #[test]
    fn load_sop_uses_config_default_execution_mode_when_omitted() {
        let dir = tempfile::tempdir().unwrap();
        let sop_dir = dir.path().join("default-mode");
        fs::create_dir_all(&sop_dir).unwrap();

        fs::write(
            sop_dir.join("SOP.toml"),
            r#"
[sop]
name = "default-mode"
description = "SOP without explicit execution mode"

[[triggers]]
type = "manual"
"#,
        )
        .unwrap();

        let sops = load_sops_from_directory(dir.path(), SopExecutionMode::Auto);
        assert_eq!(sops.len(), 1);
        assert_eq!(sops[0].execution_mode, SopExecutionMode::Auto);
    }

    #[test]
    fn validate_sop_warnings() {
        let sop = Sop {
            name: String::new(),
            description: String::new(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Supervised,
            triggers: Vec::new(),
            steps: Vec::new(),
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
        };

        let warnings = validate_sop(&sop);
        assert!(warnings.iter().any(|w| w.contains("name is empty")));
        assert!(warnings.iter().any(|w| w.contains("description is empty")));
        assert!(warnings.iter().any(|w| w.contains("no triggers")));
        assert!(warnings.iter().any(|w| w.contains("no steps")));
    }

    #[test]
    fn validate_sop_clean() {
        let sop = Sop {
            name: "valid-sop".into(),
            description: "A valid SOP".into(),
            version: "1.0.0".into(),
            priority: SopPriority::High,
            execution_mode: SopExecutionMode::Auto,
            triggers: vec![SopTrigger::Manual],
            steps: vec![SopStep {
                number: 1,
                title: "Do thing".into(),
                body: "Do the thing".into(),
                suggested_tools: vec!["shell".into()],
                requires_confirmation: false,
                kind: SopStepKind::default(),
                schema: None,
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
        };

        let warnings = validate_sop(&sop);
        assert!(warnings.is_empty());
    }

    #[test]
    fn resolve_sops_dir_default() {
        let ws = Path::new("/home/user/.zeroclaw/workspace");
        let dir = resolve_sops_dir(ws, None);
        assert_eq!(dir, ws.join("sops"));
    }

    #[test]
    fn resolve_sops_dir_override() {
        let ws = Path::new("/home/user/.zeroclaw/workspace");
        let dir = resolve_sops_dir(ws, Some("/custom/sops"));
        assert_eq!(dir, PathBuf::from("/custom/sops"));
    }

    #[test]
    fn extract_bold_title_with_dash() {
        let (title, body) = extract_bold_title("**Close valve** — Set GPIO pin LOW.").unwrap();
        assert_eq!(title, "Close valve");
        assert_eq!(body, "Set GPIO pin LOW.");
    }

    #[test]
    fn extract_bold_title_no_separator() {
        let (title, body) = extract_bold_title("**Close valve** Set pin LOW.").unwrap();
        assert_eq!(title, "Close valve");
        assert_eq!(body, "Set pin LOW.");
    }

    #[test]
    fn extract_bold_title_none() {
        assert!(extract_bold_title("No bold here").is_none());
    }

    #[test]
    fn parse_all_trigger_types() {
        let toml_str = r#"
[sop]
name = "multi-trigger"
description = "SOP with all trigger types"

[[triggers]]
type = "mqtt"
topic = "sensors/temp"
condition = "$.value > 90"

[[triggers]]
type = "webhook"
path = "/sop/test"

[[triggers]]
type = "cron"
expression = "0 */5 * * *"

[[triggers]]
type = "peripheral"
board = "nucleo-f401re-0"
signal = "pin_3"
condition = "> 0"

[[triggers]]
type = "manual"
"#;
        let manifest: SopManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.triggers.len(), 5);

        assert!(matches!(manifest.triggers[0], SopTrigger::Mqtt { .. }));
        assert!(matches!(manifest.triggers[1], SopTrigger::Webhook { .. }));
        assert!(matches!(manifest.triggers[2], SopTrigger::Cron { .. }));
        assert!(matches!(
            manifest.triggers[3],
            SopTrigger::Peripheral { .. }
        ));
        assert!(matches!(manifest.triggers[4], SopTrigger::Manual));
    }

    #[test]
    fn deterministic_flag_overrides_execution_mode() {
        let dir = tempfile::tempdir().unwrap();
        let sop_dir = dir.path().join("det-sop");
        fs::create_dir_all(&sop_dir).unwrap();

        fs::write(
            sop_dir.join("SOP.toml"),
            r#"
[sop]
name = "det-sop"
description = "A deterministic SOP"
deterministic = true

[[triggers]]
type = "manual"
"#,
        )
        .unwrap();

        fs::write(
            sop_dir.join("SOP.md"),
            r#"# Det SOP

## Steps

1. **Step one** — First step.
   - kind: execute

2. **Checkpoint** — Pause for approval.
   - kind: checkpoint

3. **Step three** — Final step.
"#,
        )
        .unwrap();

        let sops = load_sops_from_directory(dir.path(), SopExecutionMode::Supervised);
        assert_eq!(sops.len(), 1);

        let sop = &sops[0];
        assert_eq!(sop.name, "det-sop");
        assert_eq!(sop.execution_mode, SopExecutionMode::Deterministic);
        assert!(sop.deterministic);
        assert_eq!(sop.steps.len(), 3);
        assert_eq!(sop.steps[0].kind, SopStepKind::Execute);
        assert_eq!(sop.steps[1].kind, SopStepKind::Checkpoint);
        assert_eq!(sop.steps[2].kind, SopStepKind::Execute);
    }

    #[test]
    fn parse_steps_with_checkpoint_kind() {
        let md = r#"## Steps

1. **Read data** — Read from sensor.
   - tools: gpio_read
   - kind: execute

2. **Review** — Human review checkpoint.
   - kind: checkpoint

3. **Apply** — Apply changes.
"#;
        let steps = parse_steps(md);
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].kind, SopStepKind::Execute);
        assert_eq!(steps[1].kind, SopStepKind::Checkpoint);
        // Default kind should be Execute
        assert_eq!(steps[2].kind, SopStepKind::Execute);
    }
}
