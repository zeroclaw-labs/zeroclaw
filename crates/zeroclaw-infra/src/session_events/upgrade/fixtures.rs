//! Explicit old-schema test fixtures. Production migration uses the read-only
//! `readers` module; these APIs only seed migration tests.

pub mod session_backend;
pub mod session_store;
pub mod sqlite;
