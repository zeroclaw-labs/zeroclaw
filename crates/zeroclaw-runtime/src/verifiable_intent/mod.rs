//! Verifiable Intent (VI) — Rust-native implementation of the VI specification.

pub mod chain;
pub mod crypto;
pub mod error;
pub mod issuance;
pub mod types;
pub mod verification;

#[cfg(test)]
mod chain_tests;

pub use verification::StrictnessMode;
