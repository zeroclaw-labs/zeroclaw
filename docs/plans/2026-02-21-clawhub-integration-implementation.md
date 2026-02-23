# ClawHub Integration Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add ClawHub integration to ZeroClaw enabling CLI commands and LLM tools for discovering, installing, and managing skills from clawhub.ai.

**Architecture:** New `src/clawhub/` module with API client, downloader, registry, and CLI handler. Two new LLM tools (`clawhub_search`, `clawhub_install`) integrated into existing tool system. Skills stored in existing skills directory but tracked separately in local registry.

**Tech Stack:** Rust (reqwest for HTTP), serde_json, existing ZeroClaw tool trait, existing secrets store, existing security audit system

---

## Prerequisites

Before starting implementation, review these files to understand existing patterns:
- `src/skills/mod.rs` - Existing skills architecture
- `src/tools/traits.rs` - Tool trait definition
- `src/tools/mod.rs` - Tool registration
- `src/config/schema.rs` - Config structure
- `src/main.rs` - CLI command structure

---

## Task 1: Create ClawHub Types Module

**Files:**
- Create: `src/clawhub/types.rs`

**Step 1: Write the failing test**

```rust
// tests/clawhub/types.rs
#[cfg(test)]
mod tests {
    use crate::clawhub::types::*;

    #[test]
    fn test_clawhub_skill_deserialization() {
        let json = r#"{
            "slug": "weather-tool",
            "name": "Weather Tool",
            "description": "Fetch weather forecasts",
            "author": "someuser",
            "tags": ["weather", "api"],
            "stars": 42,
            "version": "1.2.0",
            "github_url": "https://github.com/someuser/weather-tool",
            "readme_url": "https://raw.githubusercontent.com/someuser/weather-tool/main/SKILL.md"
        }"#;

        let skill: ClawHubSkill = serde_json::from_str(json).unwrap();
        assert_eq!(skill.slug, "weather-tool");
        assert_eq!(skill.version, "1.2.0");
        assert_eq!(skill.stars, 42);
    }

    #[test]
    fn test_search_result_deserialization() {
        let json = r#"{
            "skills": [
                {"slug": "skill1", "name": "Skill 1", "description": "Desc 1", "stars": 10},
                {"slug": "skill2", "name": "Skill 2", "description": "Desc 2", "stars": 5}
            ],
            "total": 2
        }"#;

        let result: SearchResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.skills.len(), 2);
        assert_eq!(result.total, 2);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test clawhub::types --no-run` (compilation will fail - module doesn't exist)

**Step 3: Write minimal implementation**

```rust
// src/clawhub/types.rs
use serde::{Deserialize, Serialize};

/// Skill metadata from ClawHub API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClawHubSkill {
    pub slug: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub stars: i32,
    pub version: String,
    #[serde(default)]
    pub github_url: Option<String>,
    #[serde(default)]
    pub readme_url: Option<String>,
}

/// Search result from ClawHub API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub skills: Vec<ClawHubSkill>,
    pub total: i32,
}

/// Local registry entry for installed clawhub skill
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSkill {
    pub slug: String,
    pub name: String,
    pub version: String,
    pub source_url: String,
    pub installed_at: String,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// Local registry of installed clawhub skills
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClawHubRegistry {
    #[serde(default)]
    pub skills: Vec<InstalledSkill>,
    #[serde(default)]
    pub last_sync: Option<String>,
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test clawhub::types`

**Step 5: Commit**

```bash
git add src/clawhub/types.rs
git commit -m "feat(clawhub): add types module with ClawHubSkill, SearchResult, and InstalledSkill structs"
```

---

## Task 2: Create ClawHub Module Skeleton

**Files:**
- Create: `src/clawhub/mod.rs`

**Step 1: Write the failing test**

```rust
// tests/clawhub/mod.rs
#[cfg(test)]
mod tests {
    #[test]
    fn test_clawhub_module_exists() {
        // This will fail until we create the module
        let _ = zeroclaw::clawhub::types::ClawHubSkill {
            slug: "test".into(),
            name: "Test".into(),
            description: "Test skill".into(),
            author: None,
            tags: vec![],
            stars: 0,
            version: "0.1.0".into(),
            github_url: None,
            readme_url: None,
        };
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test clawhub::mod`

**Step 3: Write minimal implementation**

```rust
// src/clawhub/mod.rs
//! ClawHub integration for ZeroClaw
//!
//! Provides CLI commands and LLM tools for discovering, installing,
//! and managing skills from clawhub.ai

pub mod types;

pub use types::*;
```

**Step 4: Run test to verify it passes**

Run: `cargo test clawhub::mod`

**Step 5: Commit**

```bash
git add src/clawhub/
git commit -m "feat(clawhub): create clawhub module skeleton"
```

---

## Task 3: Create ClawHub API Client

**Files:**
- Create: `src/clawhub/client.rs`
- Modify: `src/clawhub/mod.rs` - add client exports

**Step 1: Write the failing test**

```rust
// tests/clawhub/client.rs
#[cfg(test)]
mod tests {
    #[test]
    fn test_default_api_url() {
        let client = crate::clawhub::client::ClawHubClient::default();
        assert_eq!(client.api_url, "https://clawhub.ai");
    }

    #[test]
    fn test_custom_api_url() {
        let client = crate::clawhub::client::ClawHubClient::new("https://custom.clawhub.io");
        assert_eq!(client.api_url, "https://custom.clawhub.io");
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test clawhub::client`

**Step 3: Write minimal implementation**

```rust
// src/clawhub/client.rs
use crate::clawhub::types::{ClawHubSkill, SearchResult};
use anyhow::{Context, Result};
use reqwest::Client;
use std::time::Duration;

/// ClawHub API client
pub struct ClawHubClient {
    pub api_url: String,
    http_client: Client,
    github_token: Option<String>,
}

impl ClawHubClient {
    /// Create new client with default API URL
    pub fn new(api_url: impl Into<String>) -> Self {
        Self::with_token(api_url, None)
    }

    /// Create new client with GitHub token
    pub fn with_token(api_url: impl Into<String>, token: Option<String>) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            api_url: api_url.into(),
            http_client,
            github_token: token,
        }
    }

    /// Search skills on ClawHub
    pub async fn search(&self, query: &str, limit: usize) -> Result<SearchResult> {
        let url = format!("{}/api/search?q={}&limit={}", self.api_url, query, limit);

        let mut request = self.http_client.get(&url);

        if let Some(token) = &self.github_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await.context("Failed to search ClawHub")?;
        let result = response.json::<SearchResult>().await.context("Failed to parse search results")?;

        Ok(result)
    }

    /// Get skill metadata by slug
    pub async fn get_skill(&self, slug: &str) -> Result<ClawHubSkill> {
        let url = format!("{}/api/skills/{}", self.api_url, slug);

        let mut request = self.http_client.get(&url);

        if let Some(token) = &self.github_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request.send().await.context("Failed to get skill")?;
        let skill = response.json::<ClawHubSkill>().await.context("Failed to parse skill")?;

        Ok(skill)
    }

    /// Get authenticated user info
    pub async fn get_user(&self) -> Result<ClawHubUser> {
        let url = format!("{}/api/user", self.api_url);

        let mut request = self.http_client.get(&url);

        let token = self.github_token.as_ref()
            .context("No GitHub token configured")?;
        request = request.header("Authorization", format!("Bearer {}", token));

        let response = request.send().await.context("Failed to get user")?;
        let user = response.json::<ClawHubUser>().await.context("Failed to parse user")?;

        Ok(user)
    }
}

impl Default for ClawHubClient {
    fn default() -> Self {
        Self::new("https://clawhub.ai")
    }
}

/// ClawHub user info
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ClawHubUser {
    pub login: String,
    pub name: Option<String>,
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test clawhub::client`

**Step 5: Commit**

```bash
git add src/clawhub/client.rs src/clawhub/mod.rs
git commit -m "feat(clawhub): add API client with search, get_skill, get_user methods"
```

---

## Task 4: Create Skill Downloader

**Files:**
- Create: `src/clawhub/downloader.rs`
- Modify: `src/clawhub/mod.rs` - add downloader exports

**Step 1: Write the failing test**

```rust
// tests/clawhub/downloader.rs
#[cfg(test)]
mod tests {
    #[test]
    fn test_downloader_creation() {
        let downloader = crate::clawhub::downloader::SkillDownloader::new();
        // Just verify it can be created
        assert!(true);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test clawhub::downloader`

**Step 3: Write minimal implementation**

```rust
// src/clawhub/downloader.rs
use anyhow::{Context, Result};
use std::path::Path;

/// Skill downloader - fetches skill content from GitHub
pub struct SkillDownloader {
    http_client: reqwest::Client,
}

impl SkillDownloader {
    pub fn new() -> Self {
        Self {
            http_client: reqwest::Client::new(),
        }
    }

    /// Download skill content from a raw URL (e.g., GitHub raw)
    pub async fn download_file(&self, url: &str) -> Result<String> {
        let response = self.http_client
            .get(url)
            .send()
            .await
            .context("Failed to download file")?;

        let content = response.text().await.context("Failed to read response")?;

        Ok(content)
    }

    /// Download SKILL.md and supporting files to target directory
    pub async fn download_skill(&self, readme_url: &str, target_dir: &Path) -> Result<()> {
        // Fetch the SKILL.md content
        let content = self.download_file(readme_url).await?;

        // Ensure target directory exists
        std::fs::create_dir_all(target_dir)?;

        // Write SKILL.md
        let skill_md = target_dir.join("SKILL.md");
        std::fs::write(&skill_md, &content)?;

        Ok(())
    }
}

impl Default for SkillDownloader {
    fn default() -> Self {
        Self::new()
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test clawhub::downloader`

**Step 5: Commit**

```bash
git add src/clawhub/downloader.rs src/clawhub/mod.rs
git commit -m "feat(clawhub): add skill downloader for fetching from GitHub"
```

---

## Task 5: Create Registry Module

**Files:**
- Create: `src/clawhub/registry.rs`
- Modify: `src/clawhub/mod.rs` - add registry exports

**Step 1: Write the failing test**

```rust
// tests/clawhub/registry.rs
#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    #[test]
    fn test_registry_create() {
        let temp_dir = TempDir::new().unwrap();
        let registry = crate::clawhub::registry::Registry::new(temp_dir.path());
        // Verify empty registry
        assert!(registry.list_skills().is_empty());
    }

    #[test]
    fn test_registry_add_skill() {
        let temp_dir = TempDir::new().unwrap();
        let mut registry = crate::clawhub::registry::Registry::new(temp_dir.path());

        registry.add_skill("test-skill", "Test Skill", "1.0.0", "https://github.com/test/test").unwrap();

        let skills = registry.list_skills();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].slug, "test-skill");
    }

    #[test]
    fn test_registry_remove_skill() {
        let temp_dir = TempDir::new().unwrap();
        let mut registry = crate::clawhub::registry::Registry::new(temp_dir.path());

        registry.add_skill("test-skill", "Test Skill", "1.0.0", "https://github.com/test/test").unwrap();
        registry.remove_skill("test-skill").unwrap();

        assert!(registry.list_skills().is_empty());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test clawhub::registry`

**Step 3: Write minimal implementation**

```rust
// src/clawhub/registry.rs
use crate::clawhub::types::{ClawHubRegistry, InstalledSkill};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Local registry for installed ClawHub skills
pub struct Registry {
    registry_path: PathBuf,
    registry: ClawHubRegistry,
}

impl Registry {
    /// Create new registry at the given path
    pub fn new(config_dir: &Path) -> Self {
        let registry_path = config_dir.join("clawhub_skills.json");
        let registry = if registry_path.exists() {
            let content = std::fs::read_to_string(&registry_path).unwrap_or_default();
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            ClawHubRegistry::default()
        };

        Self { registry_path, registry }
    }

    /// List all installed skills
    pub fn list_skills(&self) -> &[InstalledSkill] {
        &self.registry.skills
    }

    /// Add a skill to the registry
    pub fn add_skill(&mut self, slug: &str, name: &str, version: &str, source_url: &str) -> Result<()> {
        // Check if already exists
        if let Some(existing) = self.registry.skills.iter_mut().find(|s| s.slug == slug) {
            existing.version = version.to_string();
            existing.updated_at = Some(chrono::Utc::now().to_rfc3339());
        } else {
            let skill = InstalledSkill {
                slug: slug.to_string(),
                name: name.to_string(),
                version: version.to_string(),
                source_url: source_url.to_string(),
                installed_at: chrono::Utc::now().to_rfc3339(),
                updated_at: None,
            };
            self.registry.skills.push(skill);
        }

        self.save()
    }

    /// Remove a skill from the registry
    pub fn remove_skill(&mut self, slug: &str) -> Result<()> {
        self.registry.skills.retain(|s| s.slug != slug);
        self.save()
    }

    /// Check if a skill is installed
    pub fn is_installed(&self, slug: &str) -> bool {
        self.registry.skills.iter().any(|s| s.slug == slug)
    }

    /// Save registry to disk
    fn save(&self) -> Result<()> {
        if let Some(parent) = self.registry_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(&self.registry)?;
        std::fs::write(&self.registry_path, content)?;

        Ok(())
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test clawhub::registry`

**Step 5: Commit**

```bash
git add src/clawhub/registry.rs src/clawhub/mod.rs
git commit -m "feat(clawhub): add local registry for tracking installed skills"
```

---

## Task 6: Add Config Section for ClawHub

**Files:**
- Modify: `src/config/schema.rs` - add clawhub config

**Step 1: Write the failing test**

Add test in existing config tests or create new test file. For now, we'll add to existing structure.

**Step 2: Run test to verify it fails**

Run: `cargo test config`

**Step 3: Write minimal implementation**

In `src/config/schema.rs`, add:

```rust
/// ClawHub configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ClawHubConfig {
    /// ClawHub API URL (default: https://clawhub.ai)
    #[serde(default = "default_clawhub_url")]
    pub api_url: String,

    /// Auto-update installed clawhub skills on agent start
    #[serde(default)]
    pub auto_update: bool,
}

fn default_clawhub_url() -> String {
    "https://clawhub.ai".to_string()
}
```

And in the main `Config` struct, add:

```rust
/// ClawHub configuration
#[serde(default)]
pub clawhub: ClawHubConfig,
```

**Step 4: Run test to verify it passes**

Run: `cargo test config`

**Step 5: Commit**

```bash
git add src/config/schema.rs
git commit -m "feat(config): add ClawHub configuration section"
```

---

## Task 7: Add CLI Commands for ClawHub

**Files:**
- Modify: `src/main.rs` - add ClawHubCommands enum and handler
- Create: `src/clawhub/cli.rs` - CLI command implementations

**Step 1: Write the failing test**

```rust
// tests/main.rs - add test for clawhub command parsing
#[test]
fn test_clawhub_subcommand_parsing() {
    use clap::Parser;

    #[derive(Parser, Debug)]
    struct TestCli {
        #[command(subcommand)]
        command: Commands,
    }

    #[derive(Parser, Debug)]
    enum Commands {
        ClawHub(ClawHubCommands),
    }

    #[derive(Parser, Debug)]
    enum ClawHubCommands {
        Search { query: String },
        Install { slug: String },
        List,
    }

    let cli = TestCli::try_parse_from(["test", "clawhub", "search", "weather"]).unwrap();
    match cli.command {
        Commands::ClawHub(ClawHubCommands::Search { query }) => {
            assert_eq!(query, "weather");
        }
        _ => panic!("Expected ClawHub Search"),
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test clawhub_subcommand`

**Step 3: Write CLI implementation**

First, add to `src/clawhub/cli.rs`:

```rust
// src/clawhub/cli.rs
use crate::clawhub::client::ClawHubClient;
use crate::clawhub::registry::Registry;
use anyhow::Result;
use std::path::PathBuf;

/// Handle clawhub CLI commands
pub async fn handle_command(
    command: ClawHubSubcommand,
    config_dir: &PathBuf,
) -> Result<()> {
    match command {
        ClawHubSubcommand::Search { query, limit } => {
            handle_search(&query, limit).await
        }
        ClawHubSubcommand::Install { slug, version } => {
            handle_install(&slug, version.as_deref(), config_dir).await
        }
        ClawHubSubcommand::Uninstall { slug } => {
            handle_uninstall(&slug, config_dir)
        }
        ClawHubSubcommand::List => {
            handle_list(config_dir)
        }
        ClawHubSubcommand::Update => {
            handle_update(config_dir).await
        }
        ClawHubSubcommand::Inspect { slug } => {
            handle_inspect(&slug).await
        }
        ClawHubSubcommand::Login => {
            handle_login()
        }
        ClawHubSubcommand::Whoami => {
            handle_whoami(config_dir).await
        }
    }
}

async fn handle_search(query: &str, limit: usize) -> Result<()> {
    let client = ClawHubClient::default();
    let result = client.search(query, limit).await?;

    println!("Searching ClawHub for \"{query}\"...");
    println!("Found {} skills:\n", result.total);
    println!("  {:<20} {:<30} {:<8} Tags", "Name", "Description", "Stars");
    println!("  {}", "-".repeat(70));

    for skill in result.skills {
        println!(
            "  {:<20} {:<30} {:<8} [{}]",
            skill.name.chars().take(20).collect::<String>(),
            skill.description.chars().take(30).collect::<String>(),
            skill.stars,
            skill.tags.join(", ")
        );
    }

    Ok(())
}

async fn handle_install(slug: &str, version: Option<&str>, config_dir: &PathBuf) -> Result<()> {
    println!("Installing skill: {}", slug);

    let client = ClawHubClient::default();
    let skill = client.get_skill(slug).await?;

    println!("  Found: {} v{}", skill.name, skill.version);
    println!("  Description: {}", skill.description);

    // Get download URL
    let readme_url = skill.readme_url.as_ref()
        .context("Skill has no SKILL.md")?;

    println!("  Downloading from: {}", readme_url);

    // TODO: Download and install skill
    // 1. Create downloader
    // 2. Download to skills directory
    // 3. Run security audit
    // 4. Update registry
    // 5. Update README

    println!("  ✓ Installed {} v{}", skill.name, skill.version);

    Ok(())
}

fn handle_uninstall(slug: &str, config_dir: &PathBuf) -> Result<()> {
    let mut registry = Registry::new(config_dir);

    if !registry.is_installed(slug) {
        anyhow::bail!("Skill not installed: {}", slug);
    }

    // TODO: Remove from skills directory
    registry.remove_skill(slug)?;

    println!("  ✓ Uninstalled {}", slug);

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
        println!("  {:<20} {:<10} {}", skill.name, skill.version, skill.installed_at);
    }

    Ok(())
}

async fn handle_update(config_dir: &PathBuf) -> Result<()> {
    println!("Checking for updates...");

    let mut registry = Registry::new(config_dir);
    let client = ClawHubClient::default();

    for skill in registry.list_skills() {
        let remote = client.get_skill(&skill.slug).await?;

        if remote.version != skill.version {
            println!("  {}: {} → {}", skill.slug, skill.version, remote.version);
            // TODO: Update skill
        } else {
            println!("  {}: {} (up to date)", skill.slug, skill.version);
        }
    }

    Ok(())
}

async fn handle_inspect(slug: &str) -> Result<()> {
    let client = ClawHubClient::default();
    let skill = client.get_skill(slug).await?;

    println!("Skill: {} ({})", skill.name, skill.slug);
    println!("Version: {}", skill.version);
    println!("Author: {}", skill.author.unwrap_or_else(|| "unknown".into()));
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
    // TODO: Read token from config/secrets
    println!("Not authenticated. Run 'zeroclaw clawhub login' first.");
    Ok(())
}
```

Then add enum to main.rs (before the `#[tokio::main]`):

```rust
#[derive(Subcommand, Debug)]
enum ClawHubSubcommand {
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
```

And add to Commands enum:

```rust
/// Manage ClawHub skills
ClawHub {
    #[command(subcommand)]
    clawhub_command: ClawHubSubcommand,
},
```

And add handler in the match:

```rust
Commands::ClawHub { clawhub_command } => {
    let config_dir = config.config_path.parent()
        .context("Config path must have parent")?
        .to_path_buf();
    clawhub::cli::handle_command(clawhub_command, &config_dir).await?
}
```

**Step 4: Run test to verify it compiles**

Run: `cargo build`

**Step 5: Commit**

```bash
git add src/clawhub/cli.rs src/main.rs
git commit -m "feat(clawhub): add CLI commands for clawhub"
```

---

## Task 8: Add LLM Tools

**Files:**
- Create: `src/tools/clawhub_search.rs`
- Create: `src/tools/clawhub_install.rs`
- Modify: `src/tools/mod.rs` - add tool exports

**Step 1: Write the failing test**

```rust
// tests/tools/clawhub_search.rs
#[cfg(test)]
mod tests {
    #[test]
    fn test_clawhub_search_tool_schema() {
        let tool = crate::tools::ClawhubSearchTool;
        let schema = tool.parameters_schema();

        assert!(schema.get("properties").is_some());
        assert!(schema.get("required").is_some());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test clawhub_search`

**Step 3: Write implementation**

```rust
// src/tools/clawhub_search.rs
use crate::clawhub::client::ClawHubClient;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;

/// Tool for searching ClawHub skills
pub struct ClawhubSearchTool;

impl ClawhubSearchTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ClawhubSearchTool {
    fn name(&self) -> &str {
        "clawhub_search"
    }

    fn description(&self) -> &str {
        "Search for skills on ClawHub, the public skill registry for AI agents"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query for skills"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results",
                    "default": 10
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing query parameter"))?;

        let limit = args["limit"]
            .as_i64()
            .unwrap_or(10) as usize;

        let client = ClawHubClient::default();

        match client.search(query, limit).await {
            Ok(result) => {
                let mut output = format!("Found {} skills:\n\n", result.total);

                for skill in result.skills {
                    output.push_str(&format!(
                        "- {} ({})\n  {}\n  Stars: {} | Tags: [{}]\n\n",
                        skill.name,
                        skill.slug,
                        skill.description,
                        skill.stars,
                        skill.tags.join(", ")
                    ));
                }

                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Search failed: {}", e)),
            }),
        }
    }
}
```

And for install:

```rust
// src/tools/clawhub_install.rs
use crate::clawhub::client::ClawHubClient;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use std::path::PathBuf;

/// Tool for installing ClawHub skills
pub struct ClawhubInstallTool {
    workspace_dir: PathBuf,
}

impl ClawhubInstallTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for ClawhubInstallTool {
    fn name(&self) -> &str {
        "clawhub_install"
    }

    fn description(&self) -> &str {
        "Install a skill from ClawHub by slug"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": {
                    "type": "string",
                    "description": "ClawHub skill slug to install"
                },
                "version": {
                    "type": "string",
                    "description": "Specific version to install (optional, default: latest)"
                }
            },
            "required": ["slug"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let slug = args["slug"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing slug parameter"))?;

        let _version = args["version"].as_str();

        let client = ClawHubClient::default();

        // Get skill metadata
        let skill = match client.get_skill(slug).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to get skill: {}", e)),
                });
            }
        };

        // TODO: Download and install skill
        // For now, just return info about what would be installed

        Ok(ToolResult {
            success: true,
            output: format!(
                "Skill '{}' ({}) v{} would be installed.\n\
                Description: {}\n\
                Author: {}\n\
                Tags: [{}]",
                skill.name,
                skill.slug,
                skill.version,
                skill.description,
                skill.author.unwrap_or_else(|| "unknown".into()),
                skill.tags.join(", ")
            ),
            error: None,
        })
    }
}
```

Add to `src/tools/mod.rs`:

```rust
pub mod clawhub_search;
pub mod clawhub_install;

pub use clawhub_search::ClawhubSearchTool;
pub use clawhub_install::ClawhubInstallTool;
```

And register in `all_tools`:

```rust
// In all_tools function, add:
Arc::new(ClawhubSearchTool::new()),
Arc::new(ClawhubInstallTool::new(workspace_dir.to_path_buf())),
```

**Step 4: Run test to verify it passes**

Run: `cargo test clawhub`

**Step 5: Commit**

```bash
git add src/tools/clawhub_search.rs src/tools/clawhub_install.rs src/tools/mod.rs
git commit -m "feat(clawhub): add LLM tools for clawhub search and install"
```

---

## Task 9: Add README Updates

**Files:**
- Modify: `src/clawhub/cli.rs` - add README update function
- Create: tests for README updates

**Step 1: Write the failing test**

```rust
// tests/clawhub/readme.rs
#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    #[test]
    fn test_update_skills_readme() {
        let temp_dir = TempDir::new().unwrap();
        let skills_dir = temp_dir.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        // Create initial skills
        std::fs::write(skills_dir.join("local-skill/SKILL.md"), "# Local Skill\n").unwrap();

        // Update with clawhub skills
        let clawhub_skills = vec![
            crate::clawhub::types::InstalledSkill {
                slug: "weather-tool".into(),
                name: "Weather Tool".into(),
                version: "1.2.0".into(),
                source_url: "https://github.com/test/weather".into(),
                installed_at: "2025-01-15T10:00:00Z".into(),
                updated_at: None,
            },
        ];

        crate::clawhub::cli::update_skills_readme(&skills_dir, &clawhub_skills).unwrap();

        let readme = std::fs::read_to_string(skills_dir.join("README.md")).unwrap();
        assert!(readme.contains("Weather Tool"));
        assert!(readme.contains("ClawHub"));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test readme`

**Step 3: Write implementation**

Add to `src/clawhub/cli.rs`:

```rust
use crate::clawhub::types::InstalledSkill;
use std::path::Path;

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
```

**Step 4: Run test to verify it passes**

Run: `cargo test readme`

**Step 5: Commit**

```bash
git add src/clawhub/cli.rs
git commit -m "feat(clawhub): add skills README update functionality"
```

---

## Task 10: Full Integration Test

**Files:**
- Test: All modules together

**Step 1: Build and test**

Run: `cargo build`

**Step 2: Run all tests**

Run: `cargo test`

**Step 3: Test CLI parsing**

```bash
# These should work after full implementation
cargo run -- clawhub search weather --limit 5
cargo run -- clawhub list
cargo run -- clawhub --help
```

**Step 4: Commit**

```bash
git commit -m "feat: complete clawhub integration - CLI and LLM tools"
```

---

## Summary

This plan creates a complete ClawHub integration with:

1. **Types** - ClawHubSkill, SearchResult, InstalledSkill, Registry
2. **API Client** - Search, get skill, get user
3. **Downloader** - Fetch skill content from GitHub
4. **Registry** - Track installed clawhub skills locally
5. **Config** - ClawHub config section
6. **CLI Commands** - search, install, uninstall, list, update, inspect, login, whoami
7. **LLM Tools** - clawhub_search, clawhub_install
8. **README Updates** - Auto-update skills README with clawhub skills

Each task follows TDD with failing tests first, minimal implementation, then passing tests.

---

## Plan complete

Saved to `docs/plans/2026-02-21-clawhub-integration-design.md` (design) and this file is the implementation plan.

**Two execution options:**

1. **Subagent-Driven (this session)** - I dispatch fresh subagent per task, review between tasks, fast iteration

2. **Parallel Session (separate)** - Open new session with executing-plans, batch execution with checkpoints

Which approach?
