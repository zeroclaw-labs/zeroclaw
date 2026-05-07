//! Compile-time platform availability constants.
//!
//! The integrations registry iterates `PLATFORMS` to surface the
//! "Platforms" category. Each row is `(display_name, available)` where
//! `available` is computed from `cfg!(target_os = ...)`. There is no
//! schema for which OSes Rust can target — the strings here ARE the
//! canonical platform names.

/// `(display_name, is_available_on_this_build)` for every platform we
/// surface in the integrations catalog.
pub const PLATFORMS: &[(&str, bool)] = &[
    ("macOS", cfg!(target_os = "macos")),
    ("Linux", cfg!(target_os = "linux")),
    ("Windows", cfg!(target_os = "windows")),
];
