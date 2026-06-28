//! Registered TUI panels built on the [`crate::panel::Panel`] trait.
//!
//! Each submodule is one panel. They are wired into the mode bar by
//! [`crate::panel::register_panels`].

pub mod report;
pub mod theme;
