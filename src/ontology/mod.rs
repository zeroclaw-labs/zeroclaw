//! Palantir-style ontology layer for MoA long-term memory.
//!
//! This module implements a lightweight ontology that models the user's real world
//! as a **digital twin**: Objects (nouns), Links (relationships), Actions (verbs),
//! and Rules (automation). It sits *above* the existing `memory` module (SQLite +
//! FTS5 + vector embeddings) and provides structured, graph-aware context to the
//! LLM agent.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐
//! │  LLM Agent (brain)                               │
//! │  ┌────────────────────────────────────────────┐  │
//! │  │ ontology tools:                            │  │
//! │  │  get_context_snapshot / execute_action      │  │
//! │  │  search_objects / create_task / ...         │  │
//! │  └────────────────┬───────────────────────────┘  │
//! │                   │                              │
//! │  ┌────────────────▼───────────────────────────┐  │
//! │  │ Ontology Layer (this module)               │  │
//! │  │  - OntologyRepo (CRUD on objects/links)    │  │
//! │  │  - ActionDispatcher (route → zeroclaw)     │  │
//! │  │  - RuleEngine (post-action automation)     │  │
//! │  │  - ContextBuilder (snapshot for LLM)       │  │
//! │  └────────────────┬───────────────────────────┘  │
//! │                   │                              │
//! │  ┌────────────────▼───────────────────────────┐  │
//! │  │ Existing Memory Layer                      │  │
//! │  │  brain.db (SQLite + FTS5 + vec embeddings) │  │
//! │  │  + NEW: ontology tables in same DB         │  │
//! │  └────────────────────────────────────────────┘  │
//! │                   │                              │
//! │  ┌────────────────▼───────────────────────────┐  │
//! │  │ ZeroClaw Tool Layer (70+ tools)            │  │
//! │  │  shell, http, kakao, browser, cron, ...    │  │
//! │  └────────────────────────────────────────────┘  │
//! └──────────────────────────────────────────────────┘
//! ```
//!
//! # Extension
//!
//! To add a new Object Type, Link Type, or Action Type, update the seed data in
//! [`schema::seed_default_types`] and add any routing logic in
//! [`dispatcher::ActionDispatcher`].

pub mod context;
pub mod dispatcher;
pub mod repo;
pub mod rules;
pub mod schema;
pub mod tools;
pub mod types;

pub use context::ContextBuilder;
pub use dispatcher::ActionDispatcher;
pub use repo::OntologyRepo;
pub use rules::RuleEngine;
#[allow(unused_imports)]
pub use types::*;
