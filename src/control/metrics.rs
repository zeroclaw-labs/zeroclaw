use super::store::ControlStore;
use std::sync::Arc;

pub struct ControlMetrics {
    store: Arc<ControlStore>,
}

impl ControlMetrics {
    pub fn new(store: Arc<ControlStore>) -> Self {
        Self { store }
    }

    pub fn snapshot(&self) -> ControlMetricsSnapshot {
        let bots = self.store.list_bots().unwrap_or_default();
        let online = bots.iter().filter(|b| b.status == "online").count();
        let offline = bots.iter().filter(|b| b.status == "offline").count();
        let commands = self
            .store
            .list_commands(None, None, 1000)
            .unwrap_or_default();
        let pending = commands
            .iter()
            .filter(|c| c.status == "pending" || c.status == "pending_approval")
            .count();
        let approved = commands.iter().filter(|c| c.status == "approved").count();
        let acked = commands.iter().filter(|c| c.status == "acked").count();
        let failed = commands.iter().filter(|c| c.status == "failed").count();
        let approvals = self
            .store
            .list_approvals(Some("pending"), 1000)
            .unwrap_or_default();

        ControlMetricsSnapshot {
            bots_total: bots.len(),
            bots_online: online,
            bots_offline: offline,
            commands_total: commands.len(),
            commands_pending: pending,
            commands_approved: approved,
            commands_acked: acked,
            commands_failed: failed,
            approvals_pending: approvals.len(),
        }
    }

    pub fn prometheus_text(&self) -> String {
        let s = self.snapshot();
        format!(
            "# HELP zeroclaw_control_bots_total Total registered bots\n\
             # TYPE zeroclaw_control_bots_total gauge\n\
             zeroclaw_control_bots_total {}\n\
             # HELP zeroclaw_control_bots_online Online bots\n\
             # TYPE zeroclaw_control_bots_online gauge\n\
             zeroclaw_control_bots_online {}\n\
             # HELP zeroclaw_control_bots_offline Offline bots\n\
             # TYPE zeroclaw_control_bots_offline gauge\n\
             zeroclaw_control_bots_offline {}\n\
             # HELP zeroclaw_control_commands_total Total commands\n\
             # TYPE zeroclaw_control_commands_total gauge\n\
             zeroclaw_control_commands_total {}\n\
             # HELP zeroclaw_control_commands_pending Pending commands\n\
             # TYPE zeroclaw_control_commands_pending gauge\n\
             zeroclaw_control_commands_pending {}\n\
             # HELP zeroclaw_control_commands_acked Acknowledged commands\n\
             # TYPE zeroclaw_control_commands_acked gauge\n\
             zeroclaw_control_commands_acked {}\n\
             # HELP zeroclaw_control_commands_failed Failed commands\n\
             # TYPE zeroclaw_control_commands_failed gauge\n\
             zeroclaw_control_commands_failed {}\n\
             # HELP zeroclaw_control_approvals_pending Pending approvals\n\
             # TYPE zeroclaw_control_approvals_pending gauge\n\
             zeroclaw_control_approvals_pending {}\n",
            s.bots_total,
            s.bots_online,
            s.bots_offline,
            s.commands_total,
            s.commands_pending,
            s.commands_acked,
            s.commands_failed,
            s.approvals_pending,
        )
    }
}

#[derive(Debug, Clone)]
pub struct ControlMetricsSnapshot {
    pub bots_total: usize,
    pub bots_online: usize,
    pub bots_offline: usize,
    pub commands_total: usize,
    pub commands_pending: usize,
    pub commands_approved: usize,
    pub commands_acked: usize,
    pub commands_failed: usize,
    pub approvals_pending: usize,
}
