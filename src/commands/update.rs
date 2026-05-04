//! `zeroclaw update` — CLI wrapper around `zeroclaw_runtime::updater`.
//!
//! The pipeline lives in the runtime crate so it can be shared with the
//! gateway's `POST /api/system/update` endpoint. This file just delegates.

pub use zeroclaw_runtime::updater::{UpdateInfo, check, run};
