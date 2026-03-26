// ZeroClaw TOTP 2FA Gating Module
//
// Provides TOTP-based two-factor authentication for gating dangerous
// agent operations. Integrates with ZeroClaw's SecurityPolicy and
// approval system.
//
// Architecture:
//   core.rs        — RFC 6238 TOTP generation/verification
//   types.rs       — All data types with Zeroize support
//   encrypted.rs   — ChaCha20-Poly1305 encrypted secret store
//   config.rs      — TOML configuration schema
//   gating.rs      — Command gate engine with HMAC-signed decisions
//   users.rs       — Multi-user registry with role resolution
//   recovery.rs    — Single-use recovery code management
//   audit.rs       — SQLite audit log with hash chain
//   approval_queue.rs — Queued proposals for autonomous operations
//   autonomy.rs    — Cron/SelfHeal context classification
//   emergency.rs   — Break-glass and E-Stop handling
//   validate.rs    — Config validation and downgrade detection

pub mod types;
pub mod core;
pub mod encrypted;
pub mod config;
pub mod gating;
pub mod users;
pub mod recovery;
pub mod audit;
pub mod approval_queue;
pub mod autonomy;
pub mod emergency;
pub mod validate;

pub use config::TotpConfig;
pub use gating::CommandGate;
pub use types::{SecurityLevel, GateDecision, SignedDecision, ExecutionContext};
