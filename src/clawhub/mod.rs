// src/clawhub/mod.rs
//! ClawHub integration for ZeroClaw
//!
//! Provides CLI commands and LLM tools for discovering, installing,
//! and managing skills from clawhub.ai

pub mod cli;
pub mod client;
pub mod downloader;
pub mod registry;
pub mod types;

pub use cli::*;
