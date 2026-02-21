// src/clawhub/mod.rs
//! ClawHub integration for ZeroClaw
//!
//! Provides CLI commands and LLM tools for discovering, installing,
//! and managing skills from clawhub.ai

pub mod client;
pub mod types;

pub use client::*;
pub use types::*;
