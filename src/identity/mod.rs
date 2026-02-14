//! Identity module — portable AI identity framework
//!
//! Supports multiple identity formats:
//! - **AIEOS** (AI Entity Object Specification v1.1) — JSON-based portable identity
//! - **`OpenClaw`** (default) — Markdown files (IDENTITY.md, SOUL.md, etc.)

pub mod aieos;

pub use aieos::{AieosEntity, AieosIdentity};
