//! `zeroclaw update` — CLI wrapper around [`zeroclaw_updater`].
//!
//! The pipeline lives in its own non-optional workspace crate so the kernel
//! build (no `agent-runtime` feature) keeps `zeroclaw update`. The gateway's
//! `POST /api/system/update` calls into the same module, so behaviour is
//! identical across surfaces.

pub use zeroclaw_updater::{UpdateInfo, check, run};
