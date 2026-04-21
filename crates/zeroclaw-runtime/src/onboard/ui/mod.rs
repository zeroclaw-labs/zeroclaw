//! `OnboardUi` backends. `term` is the dialoguer-based terminal UI;
//! `quick` (headless, flag-driven) and the ratatui backend (in `zeroclaw-tui`)
//! land in later commits.

pub mod term;

pub use term::TermUi;
