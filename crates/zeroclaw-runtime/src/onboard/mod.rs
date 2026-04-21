//! Onboard orchestrator.
//!
//! Thin dispatcher above the `OnboardUi` trait (defined in
//! `zeroclaw-config::traits`). Section-scoped entry points let callers run
//! just one slice (`zeroclaw onboard channels`) or the whole flow.
//!
//! Sections are stubs in this commit. Each fills in as it's implemented.
//! Everything writes through `Config::set_prop` (or its helpers); direct
//! struct-field assignment is off-limits per the DRY contract (#5951).

use anyhow::Result;
use zeroclaw_config::schema::Config;
use zeroclaw_config::traits::OnboardUi;

pub mod ui;

/// Which slice of onboarding to run. `All` runs every section in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    All,
    Workspace,
    Providers,
    Channels,
    Memory,
    Hardware,
    Tunnel,
    Project,
}

/// Runtime knobs sourced from CLI flags. `--quick`/`--tui` select the UI
/// backend at the binary edge and don't appear here — the orchestrator only
/// cares about per-section behavior.
#[derive(Debug, Default, Clone)]
pub struct Flags {
    /// Skip "keep existing value?" confirmations; always re-prompt.
    pub force: bool,
    /// Back up the current config dir and start from `Config::default()`.
    pub reinit: bool,
    pub api_key: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub memory: Option<String>,
}

/// Top-level onboard dispatcher.
pub async fn run(
    cfg: &mut Config,
    ui: &mut dyn OnboardUi,
    section: Section,
    flags: &Flags,
) -> Result<()> {
    match section {
        Section::All => {
            workspace(cfg, ui, flags).await?;
            providers(cfg, ui, flags).await?;
            channels(cfg, ui, flags).await?;
            memory(cfg, ui, flags).await?;
            hardware(cfg, ui, flags).await?;
            tunnel(cfg, ui, flags).await?;
            project(cfg, ui, flags).await?;
        }
        Section::Workspace => workspace(cfg, ui, flags).await?,
        Section::Providers => providers(cfg, ui, flags).await?,
        Section::Channels => channels(cfg, ui, flags).await?,
        Section::Memory => memory(cfg, ui, flags).await?,
        Section::Hardware => hardware(cfg, ui, flags).await?,
        Section::Tunnel => tunnel(cfg, ui, flags).await?,
        Section::Project => project(cfg, ui, flags).await?,
    }
    Ok(())
}

// ── Section stubs ────────────────────────────────────────────────────────
// Each lands in its own commit. Bodies stay in mod.rs until one grows past
// ~50 lines, at which point it earns its own file under `sections/`.

async fn workspace(_cfg: &mut Config, _ui: &mut dyn OnboardUi, _flags: &Flags) -> Result<()> {
    Ok(())
}

async fn providers(_cfg: &mut Config, _ui: &mut dyn OnboardUi, _flags: &Flags) -> Result<()> {
    Ok(())
}

async fn channels(_cfg: &mut Config, _ui: &mut dyn OnboardUi, _flags: &Flags) -> Result<()> {
    Ok(())
}

async fn memory(_cfg: &mut Config, _ui: &mut dyn OnboardUi, _flags: &Flags) -> Result<()> {
    Ok(())
}

async fn hardware(_cfg: &mut Config, _ui: &mut dyn OnboardUi, _flags: &Flags) -> Result<()> {
    Ok(())
}

async fn tunnel(_cfg: &mut Config, _ui: &mut dyn OnboardUi, _flags: &Flags) -> Result<()> {
    Ok(())
}

async fn project(_cfg: &mut Config, _ui: &mut dyn OnboardUi, _flags: &Flags) -> Result<()> {
    Ok(())
}
