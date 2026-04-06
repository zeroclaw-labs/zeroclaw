//! Desktop automation commands for macOS.
//!
//! Each submodule exposes Tauri commands that the frontend (or AI agent via
//! the local-node bridge) can invoke.

pub mod accessibility;
pub mod applescript;
pub mod camera;
pub mod microphone;
pub mod notifications;
pub mod screen;
