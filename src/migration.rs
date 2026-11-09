pub use zeroclaw_runtime::migration::*;

use crate::config::Config;
use crate::memory::{self, Memory, MemoryCategory};
use anyhow::{Context, Result, bail};
use directories::UserDirs;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub async fn handle_command(command: crate::MigrateCommands, config: &Config) -> Result<()> {
    match command {
        crate::MigrateCommands::Openclaw {
            source,
            dry_run,
            reindex,
        } => migrate_openclaw_memory(config, source, dry_run, reindex).await,
        crate::MigrateCommands::OpenclawConfig { source, dry_run } => {
            Box::pin(migrate_openclaw_config(config, source, dry_run)).await
        }
    }
}
