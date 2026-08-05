//! Verifiable Intent (VI) — Rust-native implementation of the VI specification.

pub mod crypto;
pub mod error;
pub mod issuance;
pub mod types;
pub mod verification;

pub use verification::StrictnessMode;
