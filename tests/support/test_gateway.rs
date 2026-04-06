//! Test helper — re-exports for node E2E tests.
//!
//! Since the gateway's `AppState` requires many private constructors, the full
//! HTTP stack tests are placed inside `src/gateway/nodes.rs` as inline tests.
//! This module provides shared utilities for integration-level node tests that
//! operate directly on the public `NodeRegistry` and `NodePersistence` APIs.

// Intentionally empty — the real test gateway harness would require making
// internal constructors pub(crate) or adding a test-only factory method.
// For now, node E2E tests use the registry and persistence APIs directly.
