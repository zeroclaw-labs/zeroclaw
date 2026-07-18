//! Gitea/Forgejo provider for the git-forge channel.

mod api;
mod mapping;
mod payloads;
mod provider;

pub use provider::GiteaProvider;
