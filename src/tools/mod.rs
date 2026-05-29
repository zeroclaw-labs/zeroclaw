pub use zeroclaw_runtime::tools::*;

// --- X0 fork tools ---
//
// Two gates here:
// - `x0-extended`: tools whose imports compile cleanly against the V3 schema.
//   Activate when you want the additive X0 fork surfaces (file_read, soul,
//   wallet, NVIDIA, etc.) on top of the runtime crate's tool registry.
// - `x0-broken-legacy`: tools whose imports still reference pre-V3 items
//   (DelegateAgentConfig, TreasuryConfig, the old single-arg CronJobDecl
//   shape, the old runtime module, …). Not in `x0-extended`. Re-enable as
//   each tool is rewritten against the V3 API.

// All cron tools target an older `CronJobDecl` shape (`enabled` field on a
// HashMap, 4/5-arg constructor) that the V3 schema replaced. Rewrite-once
// to the new shape and lift the gate.
#[cfg(feature = "x0-broken-legacy")]
pub mod cron_add;
#[cfg(feature = "x0-broken-legacy")]
pub mod cron_list;
#[cfg(feature = "x0-broken-legacy")]
pub mod cron_remove;
#[cfg(feature = "x0-broken-legacy")]
pub mod cron_run;
#[cfg(feature = "x0-broken-legacy")]
pub mod cron_runs;
#[cfg(feature = "x0-broken-legacy")]
pub mod cron_update;

// `delegate` and `multi_delegate` are legacy duplicates of the V3 runtime
// crate's delegate tool; their imports reference pre-V3 schema items.
#[cfg(feature = "x0-broken-legacy")]
pub mod delegate;
#[cfg(feature = "x0-broken-legacy")]
pub mod multi_delegate;

// `schedule` uses the old 3-arg signature of the V3 cron API. Same fix
// path as the cron tools.
#[cfg(feature = "x0-broken-legacy")]
pub mod schedule;

// `shell` depends on the gated `runtime` module.
#[cfg(feature = "x0-broken-legacy")]
pub mod shell;

// `soul_replicate` depends on the gated `soul::replication`.
#[cfg(feature = "x0-broken-legacy")]
pub mod soul_replicate;

// `wallet_pay` references the removed `TreasuryConfig`.
#[cfg(feature = "x0-broken-legacy")]
pub mod wallet_pay;

// --- X0 fork tools that DO compile against V3 ---
#[cfg(feature = "x0-extended")]
pub mod browser_use;
#[cfg(feature = "x0-extended")]
pub mod file_read;
#[cfg(feature = "x0-extended")]
pub mod nvidia_cosmos;
#[cfg(feature = "x0-extended")]
pub mod nvidia_speech;
#[cfg(feature = "x0-extended")]
pub mod nvidia_triton;
#[cfg(feature = "x0-extended")]
pub mod nvidia_vision;
#[cfg(feature = "x0-extended")]
pub mod research_claw;
#[cfg(feature = "x0-extended")]
pub mod soul_reflect;
#[cfg(feature = "x0-extended")]
pub mod soul_status;
#[cfg(feature = "x0-extended")]
pub mod traits;
#[cfg(feature = "x0-extended")]
pub mod wallet_balance;
#[cfg(feature = "x0-extended")]
pub mod wallet_info;
#[cfg(feature = "x0-extended")]
pub mod wallet_send;
#[cfg(feature = "x0-extended")]
pub mod wallet_sign;
#[cfg(feature = "x0-extended")]
pub mod wallet_token_balance;
#[cfg(feature = "x0-extended")]
pub mod wallet_token_send;
