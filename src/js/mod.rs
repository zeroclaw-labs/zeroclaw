// JS Plugin Support for ZeroClaw
//
// This module provides TypeScript/JavaScript plugin execution through QuickJS.
// It is gated behind the `js` or `js-lite` feature flags.

#[cfg(any(feature = "js", feature = "js-lite", feature = "js-runtime"))]
pub mod api;
#[cfg(any(feature = "js", feature = "js-bundle"))]
pub mod bundle;
#[cfg(any(feature = "js", feature = "js-runtime", feature = "js-transpile"))]
pub mod cli;
#[cfg(any(feature = "js", feature = "js-lite", feature = "js-runtime"))]
pub mod config;
#[cfg(any(
    feature = "js",
    feature = "js-lite",
    feature = "js-runtime",
    feature = "js-transpile"
))]
pub mod error;
#[cfg(any(feature = "js", feature = "js-runtime"))]
pub mod events;
#[cfg(any(feature = "js", feature = "js-runtime"))]
pub mod hooks;
#[cfg(any(feature = "js", feature = "js-runtime", feature = "js-transpile"))]
pub mod install;
#[cfg(any(feature = "js", feature = "js-lite", feature = "js-transpile"))]
pub mod manifest;
#[cfg(any(feature = "js", feature = "js-registry"))]
pub mod registry;
#[cfg(any(feature = "js", feature = "js-lite", feature = "js-runtime"))]
pub mod runtime;
#[cfg(any(feature = "js", feature = "js-lite", feature = "js-runtime"))]
pub mod sandbox;
#[cfg(any(feature = "js", feature = "js-runtime"))]
pub mod skill;
#[cfg(any(feature = "js", feature = "js-runtime"))]
pub mod tool;
#[cfg(any(feature = "js", feature = "js-lite", feature = "js-transpile"))]
pub mod transpile;

#[cfg(test)]
mod tests;
