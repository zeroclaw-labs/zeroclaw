// src/clawhub/cli.rs
//! CLI commands for ClawHub integration

use crate::clawhub::client::ClawHubClient;
use crate::clawhub::downloader::SkillDownloader;
use crate::clawhub::registry::Registry;
use crate::clawhub::types::InstalledSkill;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// CLI subcommands for clawhub
#[derive(Debug, Clone, clap::Subcommand)]
pub enum ClawHubSubcommand {
    /// Search for skills on ClawHub
    Search {
        /// Search query
        query: String,
        /// Maximum results
        #[arg(long, default_value = "10")]
        limit: usize,
    },
    /// Install a skill from ClawHub
    Install {
        /// Skill slug to install
        slug: String,
        /// Specific version (optional)
        #[arg(long)]
        version: Option<String>,
    },
    /// Uninstall a skill
    Uninstall {
        /// Skill slug to uninstall
        slug: String,
    },
    /// List installed skills
    List,
    /// Update all installed skills
    Update,
    /// Show skill details
    Inspect {
        /// Skill slug to inspect
        slug: String,
    },
    /// Login to ClawHub
    Login,
    /// Show current user
    Whoami,
}

/// Handle clawhub CLI commands
pub async fn handle_command(
    command: ClawHubSubcommand,
    config_dir: &PathBuf,
    workspace_dir: &PathBuf,
) -> Result<()> {
    match command {
        ClawHubSubcommand::Search { query, limit } => handle_search(&query, limit).await,
        ClawHubSubcommand::Install { slug, version } => {
            handle_install(&slug, version.as_deref(), config_dir, workspace_dir).await
        }
        ClawHubSubcommand::Uninstall { slug } => handle_uninstall(&slug, config_dir, workspace_dir),
        ClawHubSubcommand::List => handle_list(config_dir),
        ClawHubSubcommand::Update => handle_update(config_dir, workspace_dir).await,
        ClawHubSubcommand::Inspect { slug } => handle_inspect(&slug).await,
        ClawHubSubcommand::Login => handle_login(),
        ClawHubSubcommand::Whoami => handle_whoami(config_dir).await,
    }
}

async fn handle_search(query: &str, limit: usize) -> Result<()> {
    let client = ClawHubClient::default();
    let skills = client.search_skills(query, limit).await?;

    println!("Searching ClawHub for \"{query}\"...");
    println!("Found {} skills:\n", skills.len());
    println!("  {:<20} {:<30} {:<8} Tags", "Name", "Description", "Stars");
    println!("  {}", "-".repeat(70));

    for skill in skills {
        println!(
            "  {:<20} {:<30} {:<8}",
            skill.name.chars().take(20).collect::<String>(),
            skill.description.chars().take(30).collect::<String>(),
            skill.stars
        );
    }

    Ok(())
}

async fn handle_install(
    slug: &str,
    _version: Option<&str>,
    config_dir: &PathBuf,
    workspace_dir: &PathBuf,
) -> Result<()> {
    println!("Installing skill: {}", slug);

    let client = ClawHubClient::default();
    let skill = client.get_skill(slug).await?;

    println!("  Found: {} v{}", skill.name, skill.version);
    println!("  Description: {}", skill.description);

    let readme_url = skill.readme_url.as_ref().context("Skill has no SKILL.md")?;

    println!("  Downloading from: {}", readme_url);

    // Download the skill content
    let downloader = SkillDownloader::new();
    let skills_path = workspace_dir.join("skills");
    std::fs::create_dir_all(&skills_path)?;

    let skill_dir = skills_path.join(slug);

    // Check if already installed
    if skill_dir.exists() {
        anyhow::bail!("Skill '{}' is already installed. Use 'zeroclaw clawhub update' to update.", slug);
    }

    // Download to temp location first for audit
    let temp_dir = std::env::temp_dir().join(format!("clawhub_install_{}", slug));
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir)?;

    // Download SKILL.md
    let content = downloader.download_file(readme_url).await?;
    let skill_md = temp_dir.join("SKILL.md");
    std::fs::write(&skill_md, &content)?;

    // Run security audit
    println!("  Running security audit...");
    match crate::skills::audit::audit_skill_directory(&temp_dir) {
        Ok(report) => {
            if !report.is_clean() {
                let _ = std::fs::remove_dir_all(&temp_dir);
                anyhow::bail!("Security audit failed: {}", report.summary());
            }
            println!("  Security audit passed ({} files scanned)", report.files_scanned);
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            anyhow::bail!("Security audit error: {}", e);
        }
    }

    // Copy to final location
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::copy(&skill_md, skill_dir.join("SKILL.md"))?;

    // Clean up temp
    let _ = std::fs::remove_dir_all(&temp_dir);

    // Update registry
    let mut registry = Registry::new(config_dir);
    registry.add_skill(
        &skill.slug,
        &skill.name,
        &skill.version,
        skill.github_url.as_deref().unwrap_or(""),
    )?;

    // Update README
    let clawhub_skills = registry.list_skills().to_vec();
    update_skills_readme(&skills_path, &clawhub_skills)?;

    println!("  ✓ Installed {} v{}", skill.name, skill.version);

    Ok(())
}

fn handle_uninstall(slug: &str, config_dir: &PathBuf, workspace_dir: &PathBuf) -> Result<()> {
    let mut registry = Registry::new(config_dir);

    if !registry.is_installed(slug) {
        anyhow::bail!("Skill not installed: {}", slug);
    }

    // Remove skill directory
    let skills_path = workspace_dir.join("skills");
    let skill_dir = skills_path.join(slug);
    if skill_dir.exists() {
        std::fs::remove_dir_all(&skill_dir)?;
    }

    registry.remove_skill(slug)?;

    // Update README
    let clawhub_skills = registry.list_skills().to_vec();
    update_skills_readme(&skills_path, &clawhub_skills)?;

    println!("  Uninstalled {}", slug);
    Ok(())
}

fn handle_list(config_dir: &PathBuf) -> Result<()> {
    let registry = Registry::new(config_dir);
    let skills = registry.list_skills();

    if skills.is_empty() {
        println!("No ClawHub skills installed.");
        return Ok(());
    }

    println!("Installed ClawHub skills ({}):\n", skills.len());
    println!("  {:<20} {:<10} {}", "Name", "Version", "Installed");
    println!("  {}", "-".repeat(50));

    for skill in skills {
        println!(
            "  {:<20} {:<10} {}",
            skill.name, skill.version, skill.installed_at
        );
    }

    Ok(())
}

async fn handle_update(config_dir: &PathBuf, _workspace_dir: &PathBuf) -> Result<()> {
    println!("Checking for updates...");

    let registry = Registry::new(config_dir);
    let client = ClawHubClient::default();

    for skill in registry.list_skills() {
        if let Ok(remote) = client.get_skill(&skill.slug).await {
            if remote.version != skill.version {
                println!("  {}: {} -> {}", skill.slug, skill.version, remote.version);
            } else {
                println!("  {}: {} (up to date)", skill.slug, skill.version);
            }
        }
    }

    Ok(())
}

async fn handle_inspect(slug: &str) -> Result<()> {
    let client = ClawHubClient::default();
    let skill = client.get_skill(slug).await?;

    println!("Skill: {} ({})", skill.name, skill.slug);
    println!("Version: {}", skill.version);
    println!(
        "Author: {}",
        skill.author
    );
    println!("Stars: {}", skill.stars);
    println!("Tags: [{}]", skill.tags.join(", "));
    println!("\nDescription:\n{}", skill.description);

    if let Some(url) = &skill.github_url {
        println!("\nGitHub: {}", url);
    }

    Ok(())
}

fn handle_login() -> Result<()> {
    println!("To authenticate with ClawHub:");
    println!("1. Go to https://github.com/settings/tokens");
    println!("2. Create a Personal Access Token (classic)");
    println!("3. Grant 'repo' scope if needed");
    println!("4. Run: zeroclaw auth paste-token --provider github --token <TOKEN>");
    println!("\nOr set ZEROCLAW_CLAWHUB_TOKEN environment variable");
    Ok(())
}

async fn handle_whoami(config_dir: &PathBuf) -> Result<()> {
    // Check for token in environment or config
    let token = std::env::var("ZEROCLAW_CLAWHUB_TOKEN").ok();

    if let Some(token) = token {
        let client = ClawHubClient::with_token("https://clawhub.ai", Some(token));
        match client.get_user().await {
            Ok(user) => {
                println!("Logged in as: {}", user.login);
                if let Some(name) = user.name {
                    println!("Name: {}", name);
                }
                return Ok(());
            }
            Err(e) => {
                println!("Failed to get user info: {}", e);
            }
        }
    }

    let registry_path = config_dir.join("clawhub_skills.json");
    if registry_path.exists() {
        println!("Token configured via file.");
    }

    println!("Not authenticated. Run 'zeroclaw clawhub login' first.");
    Ok(())
}

/// Update the skills README to include ClawHub skills
pub fn update_skills_readme(skills_dir: &Path, clawhub_skills: &[InstalledSkill]) -> Result<()> {
    let readme_path = skills_dir.join("README.md");

    let mut content = String::new();

    // Header
    content.push_str("# ZeroClaw Skills\n\n");

    // Local skills section
    content.push_str("## Local Skills\n\n");
    content.push_str("Each subdirectory is a skill. Create a `SKILL.toml` or `SKILL.md` file inside.\n\n");

    // ClawHub skills section
    if !clawhub_skills.is_empty() {
        content.push_str("## ClawHub Skills\n\n");
        content.push_str("These skills installed from [ClawHub](https://clawhub.ai):\n\n");
        content.push_str("| Skill | Version | Source |\n");
        content.push_str("|-------|---------|--------|\n");

        for skill in clawhub_skills {
            content.push_str(&format!(
                "| [{}]({}) | {} | [ClawHub](https://clawhub.ai/s/{}) |\n",
                skill.name,
                format!("skills/{}/SKILL.md", skill.slug),
                skill.version,
                skill.slug
            ));
        }

        content.push_str("\n");
    }

    // Installation instructions
    content.push_str("## Installing More Skills\n\n");
    content.push_str("```bash\n");
    content.push_str("# Search ClawHub for skills\n");
    content.push_str("zeroclaw clawhub search <query>\n\n");
    content.push_str("# Install a skill\n");
    content.push_str("zeroclaw clawhub install <slug>\n\n");
    content.push_str("# List installed skills\n");
    content.push_str("zeroclaw clawhub list\n");
    content.push_str("```\n");

    std::fs::write(&readme_path, content)?;

    Ok(())
}
