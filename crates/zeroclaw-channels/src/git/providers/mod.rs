//! Git-forge provider implementations.
//!
//! Layer: provider wiring. Each forge is a sibling module implementing
//! [`crate::git::traits::GitProvider`], gated behind its own
//! `provider-<forge>` feature so a build can include only the forges it
//! needs. A new forge (e.g. GitLab) drops in here as `pub mod gitlab;`
//! with zero edits to the generic `git` core.

#[cfg(feature = "provider-github")]
pub mod github;
