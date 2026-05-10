// src/approval/mod.rs
pub mod card;

pub use card::{WkApprovalAction, WkApprovalCard, build_approval_card};

use std::collections::HashMap;
use tokio::sync::RwLock;
use zeroclaw_api::channel::ChannelApprovalResponse;

/// Type alias for the pending approvals map.
/// Key = approval_id, Value = oneshot sender to resolve the approval.
pub type PendingApprovals =
    RwLock<HashMap<String, tokio::sync::oneshot::Sender<ChannelApprovalResponse>>>;
