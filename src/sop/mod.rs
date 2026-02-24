pub mod audit;
pub mod condition;
pub mod dispatch;
pub mod engine;
#[cfg(feature = "ampersona-gates")]
pub mod gates;
pub mod metrics;
pub mod types;

pub use audit::SopAuditLogger;
pub use engine::SopEngine;
#[cfg(feature = "ampersona-gates")]
pub use gates::GateEvalState;
pub use metrics::SopMetricsCollector;
#[allow(unused_imports)]
pub use types::{
    Sop, SopEvent, SopExecutionMode, SopPriority, SopRun, SopRunAction, SopRunStatus, SopStep,
    SopStepResult, SopStepStatus, SopTrigger, SopTriggerSource,
};

use anyhow::Result;
use std::path::{Path, PathBuf};
use tracing::warn;

use types::{SopManifest, SopMeta};

// ── SOP directory helpers ───────────────────────────────────────

/// Return the default SOPs directory: `<workspace>/sops`.
fn sops_dir(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("sops")
}

/// Resolve the SOPs directory from config, falling back to workspace default.
pub fn resolve_sops_dir(workspace_dir: &Path, config_dir: Option<&str>) -> PathBuf {
    match config_dir {
        Some(dir) if !dir.is_empty() => {
            let expanded = shellexpand::tilde(dir);
            PathBuf::from(expanded.as_ref())
        }
        _ => sops_dir(workspace_dir),
    }
}

// ── SOP loading ─────────────────────────────────────────────────

/// Load all SOPs from the configured directory.
pub fn load_sops(
    workspace_dir: &Path,
    config_dir: Option<&str>,
    default_execution_mode: SopExecutionMode,
) -> Vec<Sop> {
    let dir = resolve_sops_dir(workspace_dir, config_dir);
    load_sops_from_directory(&dir, default_execution_mode)
}

/// Load SOPs from a specific directory. Each subdirectory may contain
/// `SOP.toml` (metadata + triggers) and `SOP.md` (procedure steps).
fn load_sops_from_directory(sops_dir: &Path, default_execution_mode: SopExecutionMode) -> Vec<Sop> {
    if !sops_dir.exists() {
        return Vec::new();
    }

    let mut sops = Vec::new();

    let Ok(entries) = std::fs::read_dir(sops_dir) else {
        return sops;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let toml_path = path.join("SOP.toml");
        if !toml_path.exists() {
            continue;
        }

        match load_sop(&path, default_execution_mode) {
            Ok(sop) => sops.push(sop),
            Err(e) => {
                warn!("Failed to load SOP from {}: {e}", path.display());
            }
        }
    }

    sops.sort_by(|a, b| a.name.cmp(&b.name));
    sops
}

/// Load a single SOP from a directory containing SOP.toml and optionally SOP.md.
fn load_sop(sop_dir: &Path, default_execution_mode: SopExecutionMode) -> Result<Sop> {
    let toml_path = sop_dir.join("SOP.toml");
    let toml_content = std::fs::read_to_string(&toml_path)?;
    let manifest: SopManifest = toml::from_str(&toml_content)?;

    let md_path = sop_dir.join("SOP.md");
    let steps = if md_path.exists() {
        let md_content = std::fs::read_to_string(&md_path)?;
        parse_steps(&md_content)
    } else {
        Vec::new()
    };

    let SopMeta {
        name,
        description,
        version,
        priority,
        execution_mode,
        cooldown_secs,
        max_concurrent,
    } = manifest.sop;

    Ok(Sop {
        name,
        description,
        version,
        priority,
        execution_mode: execution_mode.unwrap_or(default_execution_mode),
        triggers: manifest.triggers,
        steps,
        cooldown_secs,
        max_concurrent,
        location: Some(sop_dir.to_path_buf()),
    })
}

// ── Markdown step parser ────────────────────────────────────────

/// Parse procedure steps from SOP.md content.
///
/// Expects a `## Steps` heading followed by numbered items (`1.`, `2.`, …).
/// Each item's first bold text (`**...**`) is the step title; the rest is body.
/// Sub-bullets `- tools:` and `- requires_confirmation: true` are parsed.
pub fn parse_steps(md: &str) -> Vec<SopStep> {
    let mut steps = Vec::new();
    let mut in_steps_section = false;
    let mut current_number: Option<u32> = None;
    let mut current_title = String::new();
    let mut current_body = String::new();
    let mut current_tools: Vec<String> = Vec::new();
    let mut current_requires_confirmation = false;

    for line in md.lines() {
        let trimmed = line.trim();

        // Detect ## Steps heading
        if trimmed.starts_with("## ") {
            if trimmed.eq_ignore_ascii_case("## steps") || trimmed.eq_ignore_ascii_case("## Steps")
            {
                in_steps_section = true;
                continue;
            }
            // Any other ## heading ends the steps section
            if in_steps_section {
                // Flush pending step
                flush_step(
                    &mut steps,
                    &mut current_number,
                    &mut current_title,
                    &mut current_body,
                    &mut current_tools,
                    &mut current_requires_confirmation,
                );
                in_steps_section = false;
            }
            continue;
        }

        if !in_steps_section {
            continue;
        }

        // Check for numbered item: `1.`, `2.`, etc.
        if let Some(rest) = parse_numbered_item(trimmed) {
            // Flush previous step
            flush_step(
                &mut steps,
                &mut current_number,
                &mut current_title,
                &mut current_body,
                &mut current_tools,
                &mut current_requires_confirmation,
            );

            let step_num = u32::try_from(steps.len())
                .unwrap_or(u32::MAX)
                .saturating_add(1);
            current_number = Some(step_num);

            // Extract title from bold text: **title** — body
            if let Some((title, body)) = extract_bold_title(rest) {
                current_title = title;
                current_body = body;
            } else {
                current_title = rest.to_string();
                current_body = String::new();
            }
            current_tools = Vec::new();
            current_requires_confirmation = false;
            continue;
        }

        // Sub-bullet parsing (only when inside a step)
        if current_number.is_some() && trimmed.starts_with("- ") {
            let bullet = trimmed.trim_start_matches("- ").trim();
            if let Some(tools_str) = bullet.strip_prefix("tools:") {
                current_tools = tools_str
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
            } else if bullet.starts_with("requires_confirmation:") {
                if let Some(val) = bullet.strip_prefix("requires_confirmation:") {
                    current_requires_confirmation = val.trim().eq_ignore_ascii_case("true");
                }
            } else {
                // Continuation body line
                if !current_body.is_empty() {
                    current_body.push('\n');
                }
                current_body.push_str(trimmed);
            }
            continue;
        }

        // Continuation line for step body
        if current_number.is_some() && !trimmed.is_empty() {
            if !current_body.is_empty() {
                current_body.push('\n');
            }
            current_body.push_str(trimmed);
        }
    }

    // Flush final step
    flush_step(
        &mut steps,
        &mut current_number,
        &mut current_title,
        &mut current_body,
        &mut current_tools,
        &mut current_requires_confirmation,
    );

    steps
}

/// Flush accumulated step state into the steps vector.
fn flush_step(
    steps: &mut Vec<SopStep>,
    number: &mut Option<u32>,
    title: &mut String,
    body: &mut String,
    tools: &mut Vec<String>,
    requires_confirmation: &mut bool,
) {
    if let Some(n) = number.take() {
        steps.push(SopStep {
            number: n,
            title: std::mem::take(title),
            body: body.trim().to_string(),
            suggested_tools: std::mem::take(tools),
            requires_confirmation: *requires_confirmation,
        });
        *body = String::new();
        *requires_confirmation = false;
    }
}

/// Try to parse `N. rest` from a line, returning `rest` if successful.
fn parse_numbered_item(line: &str) -> Option<&str> {
    let dot_pos = line.find(". ")?;
    let prefix = &line[..dot_pos];
    if prefix.chars().all(|c| c.is_ascii_digit()) && !prefix.is_empty() {
        Some(line[dot_pos + 2..].trim())
    } else {
        None
    }
}

/// Extract `**title**` from the beginning of text, returning (title, rest).
fn extract_bold_title(text: &str) -> Option<(String, String)> {
    let start = text.find("**")?;
    let after_start = start + 2;
    let end = text[after_start..].find("**")?;
    let title = text[after_start..after_start + end].to_string();

    // Rest is everything after the closing ** and any separator (— or -)
    let rest_start = after_start + end + 2;
    let rest = text[rest_start..].trim();
    let rest = rest
        .strip_prefix("—")
        .or_else(|| rest.strip_prefix("–"))
        .or_else(|| rest.strip_prefix("-"))
        .unwrap_or(rest)
        .trim();

    Some((title, rest.to_string()))
}

// ── Validation ──────────────────────────────────────────────────

/// Validate a loaded SOP and return a list of warnings.
pub fn validate_sop(sop: &Sop) -> Vec<String> {
    let mut warnings = Vec::new();

    if sop.name.is_empty() {
        warnings.push("SOP name is empty".into());
    }
    if sop.description.is_empty() {
        warnings.push("SOP description is empty".into());
    }
    if sop.triggers.is_empty() {
        warnings.push("SOP has no triggers defined".into());
    }
    if sop.steps.is_empty() {
        warnings.push("SOP has no steps (missing or empty SOP.md)".into());
    }

    // Check step numbering continuity
    for (i, step) in sop.steps.iter().enumerate() {
        let expected = u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1);
        if step.number != expected {
            warnings.push(format!(
                "Step numbering gap: expected {expected}, got {}",
                step.number
            ));
        }
        if step.title.is_empty() {
            warnings.push(format!("Step {} has an empty title", step.number));
        }
    }

    warnings
}

// ── CLI handler ─────────────────────────────────────────────────

/// Handle the `sop` CLI subcommand.
pub fn handle_command(command: crate::SopCommands, config: &crate::config::Config) -> Result<()> {
    let sops_dir_override = config.sop.sops_dir.as_deref();

    match command {
        crate::SopCommands::List => {
            let sops = load_sops(
                &config.workspace_dir,
                sops_dir_override,
                config.sop.default_execution_mode,
            );
            if sops.is_empty() {
                println!("No SOPs found.");
                println!();
                println!("  Create one: mkdir -p ~/.zeroclaw/workspace/sops/my-sop");
                println!("              # Add SOP.toml and SOP.md");
                println!();
                println!(
                    "  SOPs directory: {}",
                    resolve_sops_dir(&config.workspace_dir, sops_dir_override).display()
                );
            } else {
                println!("SOPs ({}):", sops.len());
                println!();
                for sop in &sops {
                    let triggers: Vec<String> =
                        sop.triggers.iter().map(ToString::to_string).collect();
                    println!(
                        "  {} {} [{}] — {}",
                        console::style(&sop.name).white().bold(),
                        console::style(format!("v{}", sop.version)).dim(),
                        console::style(&sop.priority).cyan(),
                        sop.description
                    );
                    println!(
                        "    Mode: {}  Steps: {}  Triggers: {}",
                        sop.execution_mode,
                        sop.steps.len(),
                        triggers.join(", ")
                    );
                    if sop.cooldown_secs > 0 {
                        println!("    Cooldown: {}s", sop.cooldown_secs);
                    }
                }
            }
            println!();
            Ok(())
        }

        crate::SopCommands::Validate { name } => {
            let sops = load_sops(
                &config.workspace_dir,
                sops_dir_override,
                config.sop.default_execution_mode,
            );
            let matching: Vec<&Sop> = if let Some(ref name) = name {
                sops.iter().filter(|s| s.name == *name).collect()
            } else {
                sops.iter().collect()
            };

            if matching.is_empty() {
                if let Some(name) = name {
                    anyhow::bail!("SOP not found: {name}");
                }
                println!("No SOPs to validate.");
                return Ok(());
            }

            let mut any_warnings = false;
            for sop in &matching {
                let warnings = validate_sop(sop);
                if warnings.is_empty() {
                    println!(
                        "  {} {} — valid",
                        console::style("✓").green().bold(),
                        sop.name
                    );
                } else {
                    any_warnings = true;
                    println!(
                        "  {} {} — {} warning(s):",
                        console::style("!").yellow().bold(),
                        sop.name,
                        warnings.len()
                    );
                    for w in &warnings {
                        println!("      {w}");
                    }
                }
            }
            println!();

            if any_warnings {
                anyhow::bail!("Validation completed with warnings");
            }
            Ok(())
        }

        crate::SopCommands::Show { name } => {
            let sops = load_sops(
                &config.workspace_dir,
                sops_dir_override,
                config.sop.default_execution_mode,
            );
            let sop = sops
                .iter()
                .find(|s| s.name == name)
                .ok_or_else(|| anyhow::anyhow!("SOP not found: {name}"))?;

            println!(
                "{} v{}",
                console::style(&sop.name).white().bold(),
                sop.version
            );
            println!("{}", sop.description);
            println!();
            println!("Priority:       {}", sop.priority);
            println!("Execution mode: {}", sop.execution_mode);
            println!("Cooldown:       {}s", sop.cooldown_secs);
            println!("Max concurrent: {}", sop.max_concurrent);
            println!();

            if !sop.triggers.is_empty() {
                println!("Triggers:");
                for trigger in &sop.triggers {
                    println!("  - {trigger}");
                }
                println!();
            }

            if !sop.steps.is_empty() {
                println!("Steps:");
                for step in &sop.steps {
                    let confirm_tag = if step.requires_confirmation {
                        " [requires confirmation]"
                    } else {
                        ""
                    };
                    println!(
                        "  {}. {}{}",
                        step.number,
                        console::style(&step.title).bold(),
                        confirm_tag
                    );
                    if !step.body.is_empty() {
                        for line in step.body.lines() {
                            println!("     {line}");
                        }
                    }
                    if !step.suggested_tools.is_empty() {
                        println!("     Tools: {}", step.suggested_tools.join(", "));
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
    use std::fs;

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
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
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
}
