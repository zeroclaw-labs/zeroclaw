//! Compile-time platform availability constants.

/// `(display_name, is_available_on_this_build)` for every platform we
/// surface in the integrations catalog.
pub const PLATFORMS: &[(&str, bool)] = &[
    ("macOS", cfg!(target_os = "macos")),
    ("Linux", cfg!(target_os = "linux")),
    ("Windows", cfg!(target_os = "windows")),
];
