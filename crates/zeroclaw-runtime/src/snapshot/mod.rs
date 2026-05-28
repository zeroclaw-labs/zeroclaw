//! Shadow-git snapshot system.
//!
//! Tracks worktree state as git tree objects (no commits, no branches) in a
//! shadow gitdir stored outside the user's repository at
//! `<data_dir>/snapshot/<project_hash>/<worktree_hash>/`. Enables /undo,
//! /revert, and per-turn checkpointing without polluting the user's history.
//!
//! Pattern adapted from claurst's `core/src/snapshot/` module. The shadow repo
//! reads the user's `.gitignore` (via `git check-ignore`) and respects a
//! 2 MB-per-file size limit for untracked content.
//!
//! Public surface:
//! - [`ShadowSnapshot::for_session`] — construct a snapshot bound to a worktree
//! - [`ShadowSnapshot::track`] — capture current state, return a tree hash
//! - [`ShadowSnapshot::diff`] / [`ShadowSnapshot::diff_full`] — compare to a hash
//! - [`ShadowSnapshot::restore`] / [`ShadowSnapshot::revert`] — replay state
//! - [`ShadowSnapshot::cleanup`] — prune objects older than 7 days
//! - [`get_or_create`] / [`remove`] — process-wide per-worktree registry

pub mod registry;
pub mod shadow;
pub mod tracker;
pub mod types;

pub use registry::{get_or_create, remove};
pub use shadow::ShadowSnapshot;
pub use tracker::spawn_tracker;
pub use types::{FileDiff, FileStatus, Patch};
