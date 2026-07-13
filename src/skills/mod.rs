#[allow(unused_imports)]
pub use zeroclaw_runtime::skills::*;

use anyhow::{Context, Result};
use std::path::PathBuf;
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};
use zeroclaw_runtime::skills::{ScaffoldOptions, SkillFrontmatter, SkillsService};

/// Resolve a `cli-*` Fluent key for skill-bundle CLI output. Under `agent-runtime`
/// (default + what CI/release build) this routes through Fluent; without it the
/// runtime i18n crate is absent, so the English `fallback` is used.
#[allow(unused_variables)]
fn mt(key: &str, fallback: &str) -> String {
    #[cfg(feature = "agent-runtime")]
    {
        zeroclaw_runtime::i18n::get_required_cli_string(key)
    }
    #[cfg(not(feature = "agent-runtime"))]
    {
        fallback.to_string() // i18n-exempt: English fallback when Fluent (agent-runtime) is disabled
    }
}

/// `mt` with `{$name}` arguments.
#[allow(unused_variables)]
fn mta(key: &str, args: &[(&str, &str)], fallback: &str) -> String {
    #[cfg(feature = "agent-runtime")]
    {
        zeroclaw_runtime::i18n::get_required_cli_string_with_args(key, args)
    }
    #[cfg(not(feature = "agent-runtime"))]
    {
        fallback.to_string() // i18n-exempt: English fallback when Fluent (agent-runtime) is disabled
    }
}

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

// The lib target sees this as dead; only the bin target calls it from main.rs.
#[allow(dead_code)]
pub async fn handle_command(
    command: crate::SkillCommands,
    config: &crate::config::Config,
) -> Result<()> {
    let workspace_dir = &config.data_dir;
    match command {
        crate::SkillCommands::List => {
            let skills = load_skills_with_config(workspace_dir, config);
            if skills.is_empty() {
                println!("{}", get_required_cli_string("cli-skills-none-installed"));
                println!();
                println!("{}", get_required_cli_string("cli-skills-create-hint"));
                println!(
                    "              echo '# My Skill' > ~/.zeroclaw/workspace/skills/my-skill/SKILL.md" // i18n-exempt: literal shell command example
                );
                println!();
                println!("{}", get_required_cli_string("cli-skills-install-hint"));
            } else {
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-skills-installed-header",
                        &[("count", &skills.len().to_string())],
                    )
                );
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
                        println!(
                            "    {}",
                            get_required_cli_string_with_args(
                                "cli-skills-tags",
                                &[("tags", &skill.tags.join(", "))],
                            )
                        );
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
        crate::SkillCommands::Install {
            source,
            no_tier_banner,
            accept_risk,
        } => handle_install(config, source, no_tier_banner, accept_risk).await,
        crate::SkillCommands::Screen { source } => handle_screen(config, source).await,
        crate::SkillCommands::Verify { name } => handle_verify(config, name),
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
        crate::SkillCommands::Add {
            name,
            bundle,
            description,
            license,
            author,
            version,
            category,
            no_scaffold,
            edit,
        } => handle_add(
            config,
            name,
            bundle,
            description,
            license,
            author,
            version,
            category,
            no_scaffold,
            edit,
        ),
        crate::SkillCommands::Edit { name, bundle, file } => {
            handle_edit(config, name, bundle, file)
        }
        crate::SkillCommands::Bundle { bundle_command } => match bundle_command {
            crate::SkillBundleCommands::List => handle_bundle_list(config),
            crate::SkillBundleCommands::Add { alias, directory } => {
                Box::pin(handle_bundle_add(config, alias, directory)).await
            }
            crate::SkillBundleCommands::Remove { alias, yes } => {
                Box::pin(handle_bundle_remove(config, alias, yes)).await
            }
            crate::SkillBundleCommands::Rename { from, to } => {
                Box::pin(handle_bundle_rename(config, from, to)).await
            }
            crate::SkillBundleCommands::Show { alias } => handle_bundle_show(config, alias),
        },
        crate::SkillCommands::Test { name, verbose } => handle_test(config, name, verbose),
    }
}

/// True when the named installed skill's receipt records a remote source.
/// Without a receipt (dev-local or pre-provenance skills) the skill is treated
/// as local and its TEST.sh is allowed to run.
fn skill_is_remote_origin(config: &crate::config::Config, skill_name: &str) -> bool {
    let install_root = config.install_root_dir();
    zeroclaw_runtime::skills::receipt::read_receipt(&install_root, skill_name)
        .is_some_and(|r| r.source.is_remote())
}

/// Run TEST.sh validation. Under the current no-OS-sandbox posture, a skill
/// whose receipt records a remote source is refused (its unaudited commands
/// would run with only environment scrubbing) — a hard refusal that the
/// sandboxed detonation work in a later RFC lifts. Local / no-receipt skills
/// keep the existing warn-and-run behavior.
fn handle_test(config: &crate::config::Config, name: Option<String>, verbose: bool) -> Result<()> {
    let workspace_dir = &config.data_dir;
    let results = if let Some(ref skill_name) = name {
        let source_path = PathBuf::from(skill_name);
        let (target, is_installed) = if source_path.exists() {
            (source_path, false)
        } else {
            (skills_dir(workspace_dir).join(skill_name), true)
        };

        if !target.exists() {
            anyhow::bail!("Skill not found: {}", skill_name);
        }
        if is_installed && skill_is_remote_origin(config, skill_name) {
            anyhow::bail!(
                "{}",
                get_required_cli_string_with_args(
                    "cli-skills-test-remote-refused",
                    &[("name", skill_name)]
                )
            );
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
        // Test all skills, skipping (with a note) any remote-origin skill.
        let skills_path = skills_dir(workspace_dir);
        let mut results = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&skills_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let skill_name = entry.file_name().to_string_lossy().into_owned();
                if !path.join("TEST.sh").exists() {
                    continue;
                }
                if skill_is_remote_origin(config, &skill_name) {
                    eprintln!(
                        "{}",
                        get_required_cli_string_with_args(
                            "cli-skills-test-remote-refused",
                            &[("name", &skill_name)]
                        )
                    );
                    continue;
                }
                results.push(testing::test_skill(&path, &skill_name, verbose)?);
            }
        }
        results
    };

    testing::print_results(&results);

    let any_failed = results.iter().any(|r| !r.failures.is_empty());
    if any_failed {
        anyhow::bail!("Some skill tests failed.");
    }
    Ok(())
}

/// Build the screening gate for an install spec: remote sources use
/// `[skills.install_screening].remote_action` (carrying any `--accept-risk`
/// override), local sources use `local_action`.
fn screening_gate_for(
    config: &crate::config::Config,
    source: &str,
    accept_risk: Option<String>,
) -> SkillScreeningGate {
    let cfg = &config.skills.install_screening;
    let is_remote = is_clawhub_source(source)
        || is_git_source(source)
        || is_registry_source(source)
        || is_extra_registry_source(source);
    if is_remote {
        SkillScreeningGate::for_remote(cfg.remote_action, accept_risk)
    } else {
        SkillScreeningGate::for_local(cfg.local_action)
    }
}

/// Dispatch an install spec to the matching installer with the screening gate
/// threaded in.
async fn dispatch_install(
    config: &crate::config::Config,
    source: &str,
    workspace_dir: &std::path::Path,
    skills_path: &std::path::Path,
    no_tier_banner: bool,
    gate: &SkillScreeningGate,
) -> Result<SkillInstallReport> {
    let allow_scripts = config.skills.allow_scripts;
    if is_clawhub_source(source) {
        install_clawhub_skill_source(source, skills_path, allow_scripts, gate)
            .await
            .with_context(|| format!("failed to install skill from ClawHub: {source}"))
    } else if is_git_source(source) {
        install_git_skill_source(source, skills_path, allow_scripts, gate)
            .with_context(|| format!("failed to install git skill source: {source}"))
    } else if is_registry_source(source) {
        println!(
            "{}",
            get_required_cli_string_with_args(
                "cli-skills-install-resolving-registry",
                &[("source", source)]
            )
        );
        install_registry_skill_source(
            source,
            skills_path,
            allow_scripts,
            workspace_dir,
            config.skills.registry_url.as_deref(),
            no_tier_banner,
            gate,
        )
        .with_context(|| format!("failed to install skill from registry: {source}"))
    } else if is_extra_registry_source(source) {
        let registry_label = parse_extra_registry_source(source)
            .map(|(name, _)| name)
            .unwrap_or_default();
        println!(
            "{}",
            get_required_cli_string_with_args(
                "cli-skills-install-resolving-extra-registry",
                &[("source", source), ("registry", &registry_label)]
            )
        );
        install_extra_registry_skill_source(
            source,
            skills_path,
            allow_scripts,
            workspace_dir,
            &config.skills.extra_registries,
            no_tier_banner,
            gate,
        )
        .with_context(|| format!("failed to install skill from extra registry: {source}"))
    } else {
        install_local_skill_source(source, skills_path, allow_scripts, gate)
            .with_context(|| format!("failed to install local skill source: {source}"))
    }
}

/// Install a skill, applying install-boundary screening. On a screening denial
/// under `confirm`, prints the report and the staged content hash, then either
/// prompts on a TTY or instructs a rerun with `--accept-risk=<hash>`.
async fn handle_install(
    config: &crate::config::Config,
    source: String,
    no_tier_banner: bool,
    accept_risk: Option<String>,
) -> Result<()> {
    let workspace_dir = &config.data_dir;
    println!(
        "{}",
        get_required_cli_string_with_args("cli-skills-install-start", &[("source", &source)])
    );

    let skills_path = skills_dir(workspace_dir);
    std::fs::create_dir_all(&skills_path)?;

    let gate = screening_gate_for(config, &source, accept_risk.clone());
    let outcome = dispatch_install(
        config,
        &source,
        workspace_dir,
        &skills_path,
        no_tier_banner,
        &gate,
    )
    .await;

    let report = match outcome {
        Ok(report) => report,
        Err(err) => {
            // A screening denial surfaces as RiskAcceptanceRequired; show the
            // report + hash and, on a TTY, offer to re-run with the override.
            if let Some(risk) = err.downcast_ref::<RiskAcceptanceRequired>() {
                return handle_screening_denial(config, &source, no_tier_banner, risk).await;
            }
            return Err(err);
        }
    };

    if let Some(screening) = &report.screening
        && !screening.is_clean()
    {
        eprint!("{}", screening.render());
    }

    record_install_receipt(config, &source, &report);
    print_install_success(&report);
    Ok(())
}

/// Handle a screening denial: print the report + staged hash. Under `block`
/// (no override possible) abort. Under `confirm`, on an interactive TTY prompt
/// y/N against the displayed hash and, if accepted, re-run the install with a
/// content-bound override; otherwise instruct a `--accept-risk` rerun.
async fn handle_screening_denial(
    config: &crate::config::Config,
    source: &str,
    no_tier_banner: bool,
    risk: &RiskAcceptanceRequired,
) -> Result<()> {
    eprint!("{}", risk.report.render());
    eprintln!(
        "{}",
        get_required_cli_string_with_args(
            "cli-skills-screen-staged-hash",
            &[("hash", &risk.staged_hash)]
        )
    );

    if risk.blocked {
        anyhow::bail!("{}", get_required_cli_string("cli-skills-screen-blocked"));
    }

    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());
    if !interactive {
        anyhow::bail!(
            "{}",
            get_required_cli_string_with_args(
                "cli-skills-screen-accept-hint",
                &[("hash", &risk.staged_hash)]
            )
        );
    }

    let proceed = dialoguer::Confirm::new()
        .with_prompt(get_required_cli_string("cli-skills-screen-confirm-prompt"))
        .default(false)
        .interact()?;
    if !proceed {
        anyhow::bail!("{}", get_required_cli_string("cli-skills-screen-declined"));
    }

    // Re-run with the content-bound override. The install re-stages and
    // re-hashes; if the source now serves different bytes the hash differs and
    // this stale override is rejected.
    let workspace_dir = &config.data_dir;
    let skills_path = skills_dir(workspace_dir);
    let gate = screening_gate_for(config, source, Some(risk.staged_hash.clone()));
    let report = dispatch_install(
        config,
        source,
        workspace_dir,
        &skills_path,
        no_tier_banner,
        &gate,
    )
    .await?;

    record_install_receipt(config, source, &report);
    print_install_success(&report);
    Ok(())
}

/// Print the standard install-success lines from a completed install report.
fn print_install_success(report: &SkillInstallReport) {
    let status = console::style("✓").green().bold().to_string();
    let installed_path = report.dir.display().to_string();
    let files_scanned = report.files_scanned.to_string();
    println!(
        "{}",
        get_required_cli_string_with_args(
            "cli-skills-install-installed-audited",
            &[
                ("status", &status),
                ("path", &installed_path),
                ("files", &files_scanned)
            ]
        )
    );
    println!(
        "{}",
        get_required_cli_string("cli-skills-install-security-audit-completed")
    );
}

/// Assemble and persist an install receipt. A write failure is a warning, not
/// a rollback — the skill is already installed.
fn record_install_receipt(
    config: &crate::config::Config,
    source: &str,
    report: &SkillInstallReport,
) {
    let install_root = config.install_root_dir();
    let name = match report.dir.file_name().and_then(|n| n.to_str()) {
        Some(name) => name.to_string(),
        None => return,
    };
    let tier_at_install = resolve_tier_label(config, source, &name);
    let receipt = zeroclaw_runtime::skills::receipt::SkillInstallReceipt {
        schema_version: zeroclaw_runtime::skills::receipt::RECEIPT_SCHEMA_VERSION,
        name: name.clone(),
        source: SkillSourceRecord::from_source(&SkillSource::parse(source)),
        immutable_resolution: report.resolution.clone(),
        tree_hash: report.tree_hash.clone(),
        tree_hash_scheme: zeroclaw_runtime::skills::receipt::TREE_HASH_SCHEME,
        version: read_installed_skill_version(&report.dir),
        tier_at_install,
        screening_ruleset_version: 0,
        screening_max_impact: None,
        screening_counts: std::collections::BTreeMap::new(),
        unscanned_count: 0,
        audit_options: format!("allow_scripts={}", config.skills.allow_scripts),
        installer_version: env!("CARGO_PKG_VERSION").to_string(),
        installed_at: install_timestamp(),
        accepted_hash: report.accepted_override.clone(),
    }
    .with_screening(report.screening.as_ref());

    if let Err(err) = zeroclaw_runtime::skills::receipt::write_receipt(&install_root, &receipt) {
        eprintln!(
            "{}",
            get_required_cli_string_with_args(
                "cli-skills-receipt-write-failed",
                &[("name", &name), ("error", &err.to_string())]
            )
        );
    }
}

/// Best-effort trust-tier label for the receipt. Registry sources resolve the
/// live registry tier; other sources have no tier and record `"unknown"`.
fn resolve_tier_label(config: &crate::config::Config, source: &str, skill_name: &str) -> String {
    if is_registry_source(source) {
        let registry_dir = config.data_dir.join("skills-registry");
        let (tier, _version) = lookup_registry_skill_tier(&registry_dir, skill_name);
        format!("{tier:?}").to_lowercase()
    } else {
        "unknown".to_string()
    }
}

/// Read the installed skill's version from its manifest, if present.
fn read_installed_skill_version(dir: &std::path::Path) -> Option<String> {
    for manifest in ["SKILL.toml", "manifest.toml"] {
        if let Ok(content) = std::fs::read_to_string(dir.join(manifest))
            && let Ok(value) = content.parse::<toml::Value>()
            && let Some(version) = value
                .get("skill")
                .and_then(|s| s.get("version"))
                .and_then(|v| v.as_str())
        {
            return Some(version.to_string());
        }
    }
    None
}

/// Current Unix timestamp in seconds for the receipt's `installed_at`.
fn install_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Verify installed skills against their receipts.
fn handle_verify(config: &crate::config::Config, name: Option<String>) -> Result<()> {
    use zeroclaw_runtime::skills::VerifyStatus;
    let install_root = config.install_root_dir();
    let skills_path = skills_dir(&config.data_dir);

    let names: Vec<String> = match name {
        Some(name) => vec![name],
        None => {
            let Ok(entries) = std::fs::read_dir(&skills_path) else {
                println!("{}", get_required_cli_string("cli-skills-none-installed"));
                return Ok(());
            };
            entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        }
    };

    let mut any_modified = false;
    for name in names {
        let skill_dir = skills_path.join(&name);
        if !skill_dir.is_dir() {
            anyhow::bail!("Skill not found: {name}");
        }
        let status = verify_skill(&install_root, &name, &skill_dir)?;
        let (glyph, key) = match status {
            VerifyStatus::Ok => (console::style("✓").green().bold(), "cli-skills-verify-ok"),
            VerifyStatus::Modified => {
                any_modified = true;
                (
                    console::style("✗").red().bold(),
                    "cli-skills-verify-modified",
                )
            }
            VerifyStatus::NoReceipt => (console::style("-").dim(), "cli-skills-verify-no-receipt"),
        };
        println!(
            "  {} {}",
            glyph,
            get_required_cli_string_with_args(key, &[("name", &name)])
        );
    }

    if any_modified {
        anyhow::bail!(
            "{}",
            get_required_cli_string("cli-skills-verify-found-modified")
        );
    }
    Ok(())
}

/// Screen a skill source without installing it. Remote sources are staged to a
/// temporary directory, scanned, and discarded; local sources are scanned in
/// place. Exits nonzero iff any `Denial` finding is present.
async fn handle_screen(config: &crate::config::Config, source: String) -> Result<()> {
    let report = if is_clawhub_source(&source)
        || is_git_source(&source)
        || is_registry_source(&source)
        || is_extra_registry_source(&source)
    {
        screen_remote_source(config, &source).await?
    } else {
        let path = PathBuf::from(&source);
        if !path.exists() {
            anyhow::bail!("Source path does not exist: {source}");
        }
        screen_skill_directory(&path)?
    };

    print!("{}", report.render());
    if report.has_denial() {
        anyhow::bail!(
            "{}",
            get_required_cli_string("cli-skills-screen-found-denial")
        );
    }
    Ok(())
}

/// Stage a remote source into a throwaway temp directory and screen it without
/// installing. Reuses the real installers with a disabled gate, pointed at a
/// temp skills dir, then screens the promoted copy.
async fn screen_remote_source(
    config: &crate::config::Config,
    source: &str,
) -> Result<zeroclaw_runtime::skills::ScreeningReport> {
    let tmp = tempfile::tempdir().context("failed to create temp dir for screening")?;
    let skills_path = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_path)?;
    let report = dispatch_install(
        config,
        source,
        tmp.path(),
        &skills_path,
        true,
        &SkillScreeningGate::disabled(),
    )
    .await?;
    screen_skill_directory(&report.dir).context("failed to screen staged skill")
}

#[allow(clippy::too_many_arguments)]
fn handle_add(
    config: &crate::config::Config,
    name: String,
    bundle: Option<String>,
    description: Option<String>,
    license: Option<String>,
    author: Option<String>,
    version: Option<String>,
    category: Option<String>,
    no_scaffold: bool,
    edit: bool,
) -> Result<()> {
    let install_root = config.install_root_dir();
    let service = SkillsService::new(config, install_root);
    let target = service
        .resolve_ref(&name, bundle.as_deref())
        .context("failed to resolve bundle target for skill add")?;

    let description = prompt_for_description(description)?;
    let frontmatter = SkillFrontmatter {
        name: target.name().to_string(),
        description,
        license,
        author,
        version: Some(version.unwrap_or_else(|| "0.1.0".to_string())),
        category,
        // Scaffold creates a tagless skill; tags (including the `slash` opt-in
        // for #7490 slash commands) are managed in the dashboard skills editor.
        tags: Vec::new(),
        // Slash options are authored in the dashboard editor, not at scaffold time.
        slash_options: Vec::new(),
    };

    let skill_dir = service.scaffold_skill(
        &target,
        frontmatter,
        ScaffoldOptions {
            create_optional_subdirs: !no_scaffold,
            body: String::new(),
        },
    )?;

    println!(
        "{}",
        zeroclaw_runtime::i18n::get_required_cli_string_with_args(
            "cli-skills-add-scaffolded",
            &[
                ("target", &target.to_string()),
                ("dir", &skill_dir.display().to_string()),
            ],
        )
    );

    if edit {
        open_in_editor(
            &skill_dir.join(zeroclaw_runtime::skills::constants::SKILL_MANIFEST_FILENAME),
        )?;
    }
    Ok(())
}

fn handle_edit(
    config: &crate::config::Config,
    name: String,
    bundle: Option<String>,
    file: Option<String>,
) -> Result<()> {
    let install_root = config.install_root_dir();
    let service = SkillsService::new(config, install_root);
    let target = service.resolve_ref(&name, bundle.as_deref())?;

    let summary = service
        .list_skills(Some(target.bundle()))?
        .into_iter()
        .find(|s| s.r#ref.name() == target.name())
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"skill_ref": target.to_string()})),
                "skill show: target ref not found"
            );
            anyhow::Error::msg(format!("skill '{target}' not found"))
        })?;

    let path = match file {
        Some(rel) => summary.directory.join(rel),
        None => summary
            .directory
            .join(zeroclaw_runtime::skills::constants::SKILL_MANIFEST_FILENAME),
    };
    if !path.exists() {
        anyhow::bail!("file not found: {}", path.display());
    }
    open_in_editor(&path)
}

/// Create a skill bundle: insert the config entry, set a custom directory if
/// given, materialize the resolved directory, and persist.
async fn handle_bundle_add(
    config: &crate::config::Config,
    alias: String,
    directory: Option<String>,
) -> Result<()> {
    let mut working = config.clone();
    if !working
        .create_map_key("skill_bundles", &alias)
        .map_err(anyhow::Error::msg)?
    {
        println!(
            "{}",
            mta(
                "cli-bundle-exists",
                &[("alias", alias.as_str())],
                "skill bundle '{$alias}' already exists (no change)"
            )
        );
        return Ok(());
    }
    if let Some(dir) = directory.as_ref()
        && let Some(b) = working.skill_bundles.get_mut(&alias)
    {
        b.directory = Some(dir.clone());
    }
    working.mark_dirty(&format!("skill_bundles.{alias}"));
    let install_root = working.install_root_dir();
    match zeroclaw_config::skill_bundles::resolve_directory(&working, &install_root, &alias) {
        Ok(dir) => {
            tokio::fs::create_dir_all(&dir).await.ok();
            let d = dir.display().to_string();
            println!(
                "{}",
                mta(
                    "cli-bundle-created",
                    &[("alias", alias.as_str()), ("dir", d.as_str())],
                    "created skill_bundles.{$alias} (dir: {$dir})"
                )
            );
        }
        Err(e) => {
            let es = e.to_string();
            println!(
                "{}",
                mta(
                    "cli-bundle-created-warn",
                    &[("alias", alias.as_str()), ("error", es.as_str())],
                    "created skill_bundles.{$alias} (warning: dir resolve failed: {$error})"
                )
            );
        }
    }
    Box::pin(working.save_dirty())
        .await
        .context("failed to persist config")
}

/// Delete a skill bundle: archive its directory, strip it from every agent's
/// `skill_bundles` list, remove the config entry, and persist. Safe-by-default:
/// without `--yes` it prints the impact and makes no change.
async fn handle_bundle_remove(
    config: &crate::config::Config,
    alias: String,
    yes: bool,
) -> Result<()> {
    let exists = config
        .get_map_keys("skill_bundles")
        .is_some_and(|k| k.contains(&alias));
    if !exists {
        anyhow::bail!(
            "{}",
            mta(
                "cli-bundle-not-configured",
                &[("alias", alias.as_str())],
                "skill bundle '{$alias}' is not configured"
            )
        );
    }
    let refs = zeroclaw_config::alias_refs::find_bundle_refs(config, &alias);
    if !yes {
        let count = refs.len().to_string();
        println!(
            "{}",
            mta(
                "cli-bundle-impact-header",
                &[("alias", alias.as_str()), ("count", count.as_str())],
                "deleting skill_bundles.{$alias} would strip it from {$count} agent reference(s):"
            )
        );
        for r in &refs {
            println!("  • {}", r.path);
        }
        println!(
            "\n{}",
            mt(
                "cli-bundle-no-changes",
                "No changes made. Re-run with --yes to apply."
            )
        );
        return Ok(());
    }
    let mut working = config.clone();
    let install_root = working.install_root_dir();
    // Resolve the bundle directory while the entry still exists, so it can be
    // archived AFTER the config change is durable.
    let bundle_dir =
        zeroclaw_config::skill_bundles::resolve_directory(&working, &install_root, &alias)
            .ok()
            .filter(|d| d.exists());

    // Mutate + PERSIST the config first, so a later archive failure can't leave
    // the config pointing at a directory already moved to _deleted/.
    let mut dirty = zeroclaw_config::alias_refs::scrub_bundle_refs(&mut working, &alias);
    working
        .delete_map_key("skill_bundles", &alias)
        .map_err(anyhow::Error::msg)?;
    dirty.push(format!("skill_bundles.{alias}"));
    for p in &dirty {
        working.mark_dirty(p);
    }
    Box::pin(working.save_dirty())
        .await
        .context("failed to persist config")?;

    // Archive the bundle directory under shared/skills/_deleted/ (the runtime
    // skips that path, so it isn't re-scanned as live skills) now that the
    // config change is on disk.
    if let Some(dir) = bundle_dir {
        let ts = chrono::Utc::now().format("%Y%m%d%H%M%S");
        let archive = install_root
            .join("shared")
            .join("skills")
            .join("_deleted")
            .join(format!("{alias}-{ts}"));
        if let Some(p) = archive.parent() {
            tokio::fs::create_dir_all(p).await.ok();
        }
        match tokio::fs::rename(&dir, &archive).await {
            Ok(()) => {
                let p = archive.display().to_string();
                println!(
                    "{}",
                    mta(
                        "cli-bundle-archived",
                        &[("path", p.as_str())],
                        "archived bundle directory → {$path}"
                    )
                );
            }
            Err(e) => {
                let es = e.to_string();
                eprintln!(
                    "{}",
                    mta(
                        "cli-bundle-warn-archive",
                        &[("error", es.as_str())],
                        "warning: bundle directory archive failed: {$error}"
                    )
                );
            }
        }
    }
    let count = refs.len().to_string();
    println!(
        "{}",
        mta(
            "cli-bundle-deleted",
            &[("alias", alias.as_str()), ("count", count.as_str())],
            "deleted skill_bundles.{$alias} (stripped from {$count} agent(s))"
        )
    );
    Ok(())
}

/// Rename a skill bundle: rename the config entry, rewrite every agent's
/// `skill_bundles` reference, move its directory, and persist.
async fn handle_bundle_rename(
    config: &crate::config::Config,
    from: String,
    to: String,
) -> Result<()> {
    let mut working = config.clone();
    let install_root = working.install_root_dir();
    // Resolve the OLD directory while the `from` entry still exists.
    let old_dir =
        zeroclaw_config::skill_bundles::resolve_directory(&working, &install_root, &from).ok();
    match working.rename_map_key("skill_bundles", &from, &to) {
        Ok(true) => {}
        Ok(false) => anyhow::bail!(
            "{}",
            mta(
                "cli-bundle-not-configured",
                &[("alias", from.as_str())],
                "skill bundle '{$alias}' is not configured"
            )
        ),
        Err(e) => {
            let es = e.to_string();
            anyhow::bail!(
                "{}",
                mta(
                    "cli-bundle-rename-failed",
                    &[("error", es.as_str())],
                    "rename failed: {$error}"
                )
            )
        }
    }
    let mut dirty = zeroclaw_config::alias_refs::rewrite_bundle_refs(&mut working, &from, &to);
    dirty.push(format!("skill_bundles.{from}"));
    dirty.push(format!("skill_bundles.{to}"));
    // Resolve the NEW directory (the entry now lives under `to`) for the move.
    let new_dir =
        zeroclaw_config::skill_bundles::resolve_directory(&working, &install_root, &to).ok();
    for p in &dirty {
        working.mark_dirty(p);
    }
    // PERSIST the config rename before moving the directory, so a later move
    // failure can't leave the config naming `to` while the dir sits at `from`.
    Box::pin(working.save_dirty())
        .await
        .context("failed to persist config")?;

    // Move the directory (default per-alias path only; a custom path is
    // alias-independent → old == new → skip).
    if let (Some(old), Some(new)) = (old_dir, new_dir)
        && old != new
        && old.exists()
    {
        if let Some(p) = new.parent() {
            tokio::fs::create_dir_all(p).await.ok();
        }
        if let Err(e) = tokio::fs::rename(&old, &new).await {
            let es = e.to_string();
            eprintln!(
                "{}",
                mta(
                    "cli-bundle-warn-move",
                    &[("error", es.as_str())],
                    "warning: bundle directory move failed: {$error}"
                )
            );
        }
    }
    println!(
        "{}",
        mta(
            "cli-bundle-renamed",
            &[("from", from.as_str()), ("to", to.as_str())],
            "renamed skill_bundles.{$from} → skill_bundles.{$to}"
        )
    );
    Ok(())
}

fn print_bundle_include_exclude(include: &[String], exclude: &[String]) {
    if !include.is_empty() {
        println!(
            "  {}",
            zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                "cli-skills-bundle-include",
                &[("values", &include.join(", "))],
            )
        );
    }
    if !exclude.is_empty() {
        println!(
            "  {}",
            zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                "cli-skills-bundle-exclude",
                &[("values", &exclude.join(", "))],
            )
        );
    }
}

fn handle_bundle_list(config: &crate::config::Config) -> Result<()> {
    let install_root = config.install_root_dir();
    let service = SkillsService::new(config, install_root);
    let bundles = service.list_bundles()?;
    if bundles.is_empty() {
        println!(
            "{}",
            zeroclaw_runtime::i18n::get_required_cli_string("cli-skills-bundle-list-empty")
        );
        return Ok(());
    }
    println!(
        "{}",
        zeroclaw_runtime::i18n::get_required_cli_string_with_args(
            "cli-skills-bundle-list-header",
            &[("count", &bundles.len().to_string())],
        )
    );
    for b in &bundles {
        println!(
            "  {}",
            zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                "cli-skills-bundle-entry",
                &[
                    ("alias", &b.alias),
                    ("dir", &b.directory.display().to_string()),
                ],
            )
        );
        print_bundle_include_exclude(&b.include, &b.exclude);
    }
    Ok(())
}

fn handle_bundle_show(config: &crate::config::Config, alias: String) -> Result<()> {
    let install_root = config.install_root_dir();
    let service = SkillsService::new(config, install_root);
    let bundles = service.list_bundles()?;
    let bundle = bundles
        .into_iter()
        .find(|b| b.alias == alias)
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"skill_bundle": alias})),
                "skill bundle lookup failed: alias not in config"
            );
            anyhow::Error::msg(format!("skill bundle '{alias}' not configured"))
        })?;

    println!(
        "{}",
        zeroclaw_runtime::i18n::get_required_cli_string_with_args(
            "cli-skills-bundle-entry",
            &[
                ("alias", &bundle.alias),
                ("dir", &bundle.directory.display().to_string()),
            ],
        )
    );
    print_bundle_include_exclude(&bundle.include, &bundle.exclude);

    let skills = service.list_skills(Some(&alias))?;
    if skills.is_empty() {
        println!(
            "  {}",
            zeroclaw_runtime::i18n::get_required_cli_string("cli-skills-bundle-show-no-skills")
        );
    } else {
        println!(
            "  {}",
            zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                "cli-skills-bundle-show-skills-header",
                &[("count", &skills.len().to_string())],
            )
        );
        for s in &skills {
            println!(
                "    {}",
                zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                    "cli-skills-bundle-show-skill",
                    &[
                        ("name", s.r#ref.name()),
                        ("description", &s.frontmatter.description),
                    ],
                )
            );
        }
    }
    Ok(())
}

fn prompt_for_description(description: Option<String>) -> Result<String> {
    if let Some(d) = description
        && !d.trim().is_empty()
    {
        return Ok(d);
    }
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        let prompt: String = dialoguer::Input::new()
            .with_prompt("Skill description (what it does, when to use it)")
            .interact_text()?;
        if prompt.trim().is_empty() {
            anyhow::bail!("description must not be empty");
        }
        Ok(prompt)
    } else {
        anyhow::bail!("--description is required when stdin is not a TTY");
    }
}

fn open_in_editor(path: &std::path::Path) -> Result<()> {
    let Some(editor) = editor_from_env_or_path() else {
        anyhow::bail!("no editor found; set VISUAL or EDITOR");
    };
    let status = std::process::Command::new(&editor).arg(path).status()?;
    if !status.success() {
        anyhow::bail!("{editor} exited with non-zero status");
    }
    Ok(())
}

fn editor_from_env_or_path() -> Option<String> {
    std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| {
            fallback_editors()
                .iter()
                .copied()
                .find(|candidate| executable_on_path(candidate))
                .map(str::to_string)
        })
}

fn executable_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
}

fn fallback_editors() -> &'static [&'static str] {
    if cfg!(windows) {
        &["notepad.exe", "nano", "vim"]
    } else {
        &["nano", "vi", "vim", "editor"]
    }
}
