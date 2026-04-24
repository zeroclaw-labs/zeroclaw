#[allow(unused_imports)]
pub use zeroclaw_runtime::skills::*;

use anyhow::{Context, Result};
use std::path::PathBuf;
pub mod creator {
    #[allow(unused_imports)]
    pub use zeroclaw_runtime::skills::creator::*;
}
pub mod audit {
    #[allow(unused_imports)]
    pub use zeroclaw_runtime::skills::audit::*;
}
pub mod skill_tool {
    #[allow(unused_imports)]
    pub use zeroclaw_runtime::skills::skill_tool::*;
}
pub mod skill_http {
    #[allow(unused_imports)]
    pub use zeroclaw_runtime::skills::skill_http::*;
}

#[allow(dead_code)]
pub fn handle_command(command: crate::SkillCommands, config: &crate::config::Config) -> Result<()> {
    let workspace_dir = &config.workspace_dir;
    match command {
        crate::SkillCommands::List => {
            let skills = load_skills_with_config(workspace_dir, config);
            if skills.is_empty() {
                println!("No skills installed.");
                println!();
                println!("  Create one: mkdir -p ~/.zeroclaw/workspace/skills/my-skill");
                println!(
                    "              echo '# My Skill' > ~/.zeroclaw/workspace/skills/my-skill/SKILL.md"
                );
                println!();
                println!("  Or install: zeroclaw skills install <source>");
            } else {
                println!("Installed skills ({}):", skills.len());
                println!();
                for skill in &skills {
                    println!(
                        "  {} {} — {}",
                        console::style(&skill.name).white().bold(),
                        console::style(format!("v{}", skill.version)).dim(),
                        skill.description
                    );
                    if !skill.tools.is_empty() {
                        println!(
                            "    Tools: {}",
                            skill
                                .tools
                                .iter()
                                .map(|t| t.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                    if !skill.tags.is_empty() {
                        println!("    Tags:  {}", skill.tags.join(", "));
                    }
                }
            }
            println!();
            Ok(())
        }
        crate::SkillCommands::Audit { source } => {
            let source_path = PathBuf::from(&source);
            let target = if source_path.exists() {
                source_path
            } else {
                skills_dir(workspace_dir).join(&source)
            };

            if !target.exists() {
                anyhow::bail!("Skill source or installed skill not found: {source}");
            }

            let report = audit::audit_skill_directory_with_options(
                &target,
                audit::SkillAuditOptions {
                    allow_scripts: config.skills.allow_scripts,
                },
            )?;
            if report.is_clean() {
                println!(
                    "  {} Skill audit passed for {} ({} files scanned).",
                    console::style("✓").green().bold(),
                    target.display(),
                    report.files_scanned
                );
                return Ok(());
            }

            println!(
                "  {} Skill audit failed for {}",
                console::style("✗").red().bold(),
                target.display()
            );
            for finding in report.findings {
                println!("    - {finding}");
            }
            anyhow::bail!("Skill audit failed.");
        }
        crate::SkillCommands::Install { source } => {
            println!("Installing skill from: {source}");

            let skills_path = skills_dir(workspace_dir);
            std::fs::create_dir_all(&skills_path)?;

            let (installed_dir, files_scanned) = if is_clawhub_source(&source) {
                install_clawhub_skill_source(&source, &skills_path, config.skills.allow_scripts)
                    .with_context(|| format!("failed to install skill from ClawHub: {source}"))?
            } else if is_git_source(&source) {
                install_git_skill_source(&source, &skills_path, config.skills.allow_scripts)
                    .with_context(|| format!("failed to install git skill source: {source}"))?
            } else if is_registry_source(&source) {
                println!("  Resolving '{source}' from skills registry...");
                install_registry_skill_source(
                    &source,
                    &skills_path,
                    config.skills.allow_scripts,
                    workspace_dir,
                    config.skills.registry_url.as_deref(),
                )
                .with_context(|| format!("failed to install skill from registry: {source}"))?
            } else {
                install_local_skill_source(&source, &skills_path, config.skills.allow_scripts)
                    .with_context(|| format!("failed to install local skill source: {source}"))?
            };
            println!(
                "  {} Skill installed and audited: {} ({} files scanned)",
                console::style("✓").green().bold(),
                installed_dir.display(),
                files_scanned
            );

            println!("  Security audit completed successfully.");
            Ok(())
        }
        crate::SkillCommands::Remove { name } => {
            // Reject path traversal attempts
            if name.contains("..") || name.contains('/') || name.contains('\\') {
                anyhow::bail!("Invalid skill name: {name}");
            }

            let skill_path = skills_dir(workspace_dir).join(&name);

            // Verify the resolved path is actually inside the skills directory
            let canonical_skills = skills_dir(workspace_dir)
                .canonicalize()
                .unwrap_or_else(|_| skills_dir(workspace_dir));
            if let Ok(canonical_skill) = skill_path.canonicalize() {
                if !canonical_skill.starts_with(&canonical_skills) {
                    anyhow::bail!("Skill path escapes skills directory: {name}");
                }
            }

            if !skill_path.exists() {
                anyhow::bail!("Skill not found: {name}");
            }

            std::fs::remove_dir_all(&skill_path)?;
            println!(
                "  {} Skill '{}' removed.",
                console::style("✓").green().bold(),
                name
            );
            Ok(())
        }
        crate::SkillCommands::Test { name, verbose } => {
            let results = if let Some(ref skill_name) = name {
                // Test a single skill
                let source_path = PathBuf::from(skill_name);
                let target = if source_path.exists() {
                    source_path
                } else {
                    skills_dir(workspace_dir).join(skill_name)
                };

                if !target.exists() {
                    anyhow::bail!("Skill not found: {}", skill_name);
                }

                let r = testing::test_skill(&target, skill_name, verbose)?;
                if r.tests_run == 0 {
                    println!(
                        "  {} No TEST.sh found for skill '{}'.",
                        console::style("-").dim(),
                        skill_name,
                    );
                    return Ok(());
                }
                vec![r]
            } else {
                // Test all skills
                let dirs = vec![skills_dir(workspace_dir)];
                testing::test_all_skills(&dirs, verbose)?
            };

            testing::print_results(&results);

            let any_failed = results.iter().any(|r| !r.failures.is_empty());
            if any_failed {
                anyhow::bail!("Some skill tests failed.");
            }
            Ok(())
        }
    }
}
