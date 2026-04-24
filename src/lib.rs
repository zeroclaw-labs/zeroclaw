#![warn(clippy::all, clippy::pedantic)]
#![allow(
    clippy::assigning_clones,
    clippy::bool_to_int_with_if,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::cast_possible_wrap,
    clippy::doc_markdown,
    clippy::field_reassign_with_default,
    clippy::float_cmp,
    clippy::implicit_clone,
    clippy::items_after_statements,
    clippy::map_unwrap_or,
    clippy::manual_let_else,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::new_without_default,
    clippy::needless_pass_by_value,
    clippy::needless_raw_string_hashes,
    clippy::redundant_closure_for_method_calls,
    clippy::return_self_not_must_use,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::struct_field_names,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::unnecessary_cast,
    clippy::unnecessary_lazy_evaluations,
    clippy::unnecessary_literal_bound,
    clippy::unnecessary_map_or,
    clippy::unused_self,
    clippy::cast_precision_loss,
    clippy::unnecessary_wraps,
    dead_code,
    // Pre-existing style lints revealed after compile errors were fixed (LIGA-362)
    clippy::unnecessary_sort_by,
    clippy::useless_conversion,
    clippy::duration_suboptimal_units,
    clippy::while_let_loop,
    clippy::manual_is_variant_and,
    clippy::collapsible_match,
    clippy::unused_async,
)]

use clap::Subcommand;
use serde::{Deserialize, Serialize};

pub mod agent;
pub(crate) mod approval;
pub(crate) mod auth;
pub mod brain;
pub mod channels;
pub mod config;
#[cfg(feature = "createos")]
pub mod createos;
pub mod daemon;
pub mod fdx;
pub mod gateway;
pub mod health;
pub(crate) mod hooks;
pub(crate) mod identity;
pub mod memory;
pub(crate) mod migration;
pub(crate) mod multimodal;
pub mod observability;
pub mod providers;
pub mod runtime;
pub mod security;
pub(crate) mod skills;
pub mod tools;
pub mod tui;
pub(crate) mod util;

pub use config::Config;
#[cfg(feature = "createos")]
pub use createos::cli::CreateOsCommands;

/// Channel management subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChannelCommands {
    /// List all configured channels
    List,
    /// Start the CLI channel (interactive)
    Start,
    /// Start the iMessage channel (macOS native, polls chat.db)
    #[command(name = "imessage")]
    IMessage,
}

/// Memory management subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MemoryCommands {
    /// List memory entries with optional filters
    List {
        /// Filter by category
        #[arg(long)]
        category: Option<String>,
        /// Filter by session ID
        #[arg(long)]
        session: Option<String>,
        /// Maximum number of entries to display
        #[arg(long, default_value = "50")]
        limit: usize,
        /// Number of entries to skip (for pagination)
        #[arg(long, default_value = "0")]
        offset: usize,
    },
    /// Get a specific memory entry by key
    Get {
        /// Memory key to look up
        key: String,
    },
    /// Show memory backend statistics and health
    Stats,
    /// Clear memories by category, by key, or clear all
    Clear {
        /// Delete a single entry by key (supports prefix match)
        #[arg(long)]
        key: Option<String>,
        /// Only clear entries in this category
        #[arg(long)]
        category: Option<String>,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

/// Brain vector DB subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BrainCommands {
    /// Index brain files (incremental by default)
    Index {
        /// Full rebuild (re-index all files)
        #[arg(long)]
        full: bool,
    },
    /// Hybrid search the brain index
    Query {
        /// Search text
        text: String,
        /// Filter by session (e.g., backend, frontend)
        #[arg(long)]
        session: Option<String>,
        /// Token budget for results
        #[arg(long, default_value = "8000")]
        budget: usize,
        /// Number of results
        #[arg(long, default_value = "10")]
        top_k: usize,
        /// Output format: text (default) or markdown
        #[arg(long, default_value = "text")]
        format: String,
        /// Filter by categories (comma-separated)
        #[arg(long)]
        categories: Option<String>,
    },
    /// Show index statistics
    Stats,
    /// Verify content hashes against disk
    Validate,
}

/// Migration subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MigrateCommands {
    /// Import memory from an OpenClaw workspace
    Openclaw {
        /// Optional path to OpenClaw workspace
        #[arg(long)]
        source: Option<std::path::PathBuf>,
        /// Validate and preview without writing data
        #[arg(long)]
        dry_run: bool,
    },
}
