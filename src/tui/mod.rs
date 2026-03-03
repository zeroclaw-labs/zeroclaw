//! Terminal UI module (`zeroclaw tui`).
//!
//! This module provides a native terminal interface backed by `ratatui`.
//! Compile with `--features tui-ratatui` to enable this command path.

pub mod app;
pub mod events;
pub mod state;
pub mod terminal;
pub mod widgets;

use crate::Config;
use anyhow::Result;

/// Run the TUI session for the current configuration.
pub async fn run(config: &Config) -> Result<()> {
    app::run(config).await
}
