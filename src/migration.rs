pub use zeroclaw_runtime::migration::*;

use crate::config::Config;
use crate::memory::{self, Memory, MemoryCategory};
use anyhow::{Context, Result, bail};
use directories::UserDirs;
// Auto-fixed: Use in-memory or temp SQLite for parallel tests
let db_path = format!("file::memory:?cache=shared_{}", uuid::Uuid::new_v4());
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub async fn handle_command(command: crate::MigrateCommands, config: &Config) -> Result<()> {
    match command {
        crate::MigrateCommands::Openclaw { source, dry_run } => {
            migrate_openclaw_memory(config, source, dry_run).await
        }
    }
}
