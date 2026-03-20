//! Verifiable Intent (VI) — Rust-native implementation of the VI specification.
//!
//! This module provides full lifecycle support for the Verifiable Intent layered
//! credential system: issuance of L2/L3 credentials, chain verification, and
//! constraint evaluation for commerce-gated agent actions.
//!
//! # Attribution
//!
//! This implementation is based on the Verifiable Intent open specification and
//! reference implementation by agent-intent, available at
//! <https://github.com/agent-intent/verifiable-intent>, licensed under the
//! Apache License, Version 2.0. This Rust-native reimplementation follows the
//! VI specification design (SD-JWT layered credentials, constraint model,
//! three-layer chain) without copying source code from the reference
//! implementation.
//!
//! # Architecture
//!
//! - [`types`] — Core data models (credentials, mandates, constraints, keys).
//! - [`crypto`] — SD-JWT / KB-SD-JWT construction and verification primitives.
//! - [`verification`] — Chain verification, constraint checking, binding integrity.
//! - [`issuance`] — L2/L3 credential construction.
//! - [`error`] — Machine-readable error taxonomy for policy decisions.
//!
//! # Extension
//!
//! This module is an internal subsystem. Integration into the tool execution
//! surface is handled by the tool layer (see `src/tools/`). Config schema
//! entries live in `src/config/schema.rs`.

pub mod crypto;
pub mod error;
pub mod issuance;
pub mod types;
pub mod verification;

pub use verification::StrictnessMode;
