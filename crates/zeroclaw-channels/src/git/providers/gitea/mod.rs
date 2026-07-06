//! Gitea/Forgejo provider for the git-forge channel.
//!
//! Implements [`crate::git::traits::GitProvider`] for the Gitea-compatible
//! REST API used by both Gitea and Forgejo. Authentication is a long-lived
//! personal access token; the instance base URL is configured per channel.

mod api;
mod mapping;
mod payloads;
mod provider;

pub use provider::GiteaProvider;
