use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use tokio::{task::JoinHandle, time::Instant};
use zeroclaw_api::cost::{ConversationCostReason, ConversationCostStatus, ConversationCostSummary};

use crate::client::RpcClient;

/// A disposable view of the server's ledger. Recreated with the RPC transport.
#[derive(Default)]
pub(crate) struct ConversationCost {
    session_id: Option<String>,
    request: Option<JoinHandle<anyhow::Result<ConversationCostSummary>>>,
    summary: Option<ConversationCostSummary>,
    next_poll: Option<Instant>,
    failures: usize,
    unsupported: bool,
    denied: bool,
    was_running: bool,
    refresh_pending: bool,
}

impl Drop for ConversationCost {
    fn drop(&mut self) {
        if let Some(request) = self.request.take() {
            request.abort();
        }
    }
}

impl ConversationCost {
    pub fn tick(&mut self, session_id: Option<&str>, running: bool, rpc: &Arc<RpcClient>) {
        if self.session_id.as_deref() != session_id {
            if let Some(request) = self.request.take() {
                request.abort();
            }
            self.session_id = session_id.map(str::to_owned);
            self.summary = None;
            self.failures = 0;
            self.next_poll = None;
            self.refresh_pending = false;
            self.was_running = running;
        }
        let Some(session_id) = session_id else {
            return;
        };
        if self.was_running && !running {
            self.next_poll = None;
            self.refresh_pending = self.request.is_some();
        }
        self.was_running = running;
        if let Some(result) = self
            .request
            .as_mut()
            .and_then(|request| request.now_or_never())
        {
            self.request = None;
            match result
                .map_err(anyhow::Error::from)
                .and_then(|result| result)
            {
                Ok(summary) => {
                    self.summary = Some(summary);
                    self.failures = 0;
                }
                Err(error) => {
                    self.summary = None;
                    self.failures = self.failures.saturating_add(1);
                    if let Some(error) = error.downcast_ref::<crate::jsonrpc::JsonRpcError>() {
                        use zeroclaw_api::jsonrpc::error_codes::*;
                        self.unsupported |= error.code == METHOD_NOT_FOUND;
                        if matches!(error.code, AUTH_REQUIRED | FORBIDDEN | SESSION_NOT_OWNED) {
                            self.denied = true;
                        }
                    }
                }
            }
            let delay = if self.failures > 0 {
                [5, 10, 20, 30][(self.failures - 1).min(3)]
            } else if self.refresh_pending {
                0
            } else {
                2
            };
            self.refresh_pending = false;
            self.next_poll = Some(Instant::now() + Duration::from_secs(delay));
        }
        if self.request.is_some()
            || self.unsupported
            || self.denied
            || self
                .next_poll
                .is_some_and(|deadline| Instant::now() < deadline)
        {
            return;
        }
        let rpc = Arc::clone(rpc);
        let session_id = session_id.to_owned();
        self.request = Some(tokio::spawn(
            async move { rpc.session_cost(&session_id).await },
        ));
    }

    pub fn label(&self) -> String {
        label(self.summary.as_ref())
    }

    pub fn detail(&self) -> String {
        let mut parts = vec![crate::i18n::t("zc-cost-detail")];
        if let Some(summary) = &self.summary {
            for reason in &summary.reasons {
                let key = match reason {
                    ConversationCostReason::MissingUsage => "zc-cost-missing-usage",
                    ConversationCostReason::MissingPrices => "zc-cost-missing-prices",
                    ConversationCostReason::UnverifiedHistory => "zc-cost-unverified-history",
                    ConversationCostReason::TrackingDisabled => "zc-cost-tracking-disabled",
                    ConversationCostReason::TrackingUnavailable => "zc-cost-tracking-unavailable",
                    ConversationCostReason::NoRecordedUsage => "zc-cost-no-recorded-usage",
                    ConversationCostReason::LedgerIntegrity => "zc-cost-ledger-integrity",
                    ConversationCostReason::PersistenceUncertain => "zc-cost-persistence-uncertain",
                };
                parts.push(crate::i18n::t(key));
            }
        }
        parts.join(" ")
    }
}

pub(crate) fn label(summary: Option<&ConversationCostSummary>) -> String {
    let Some(summary) = summary else {
        return crate::i18n::t("zc-cost-unavailable");
    };
    let Some(cost) = summary
        .estimated_cost_usd
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
        .filter(|_| summary.status != ConversationCostStatus::Unavailable)
    else {
        return crate::i18n::t("zc-cost-unavailable");
    };
    let amount = if cost > 0.0 && cost < 0.0001 {
        "<$0.0001".to_owned()
    } else {
        format!("${cost:.4}")
    };
    let estimate = crate::i18n::t_args("zc-cost-estimate", &[("amount", &amount)]);
    if summary.status == ConversationCostStatus::Partial {
        crate::i18n::t_args("zc-cost-incomplete", &[("estimate", &estimate)])
    } else {
        estimate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonrpc::{JsonRpcError, RpcOutbound};
    use serde_json::{Value, json};
    use tokio::sync::mpsc;

    fn client() -> (Arc<RpcClient>, Arc<RpcOutbound>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel(16);
        let outbound = Arc::new(RpcOutbound::new(tx));
        (
            Arc::new(RpcClient::with_rpc(Arc::clone(&outbound))),
            outbound,
            rx,
        )
    }

    async fn request(rx: &mut mpsc::Receiver<String>) -> Value {
        serde_json::from_str(&rx.recv().await.unwrap()).unwrap()
    }

    async fn settle() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn conversation_cost_polling_is_single_flight_and_continues_while_idle() {
        let (rpc, outbound, mut rx) = client();
        let mut view = ConversationCost::default();
        view.tick(Some("a"), true, &rpc);
        let first = request(&mut rx).await;
        assert_eq!(first["method"], "session/cost");
        for _ in 0..5 {
            view.tick(Some("a"), true, &rpc);
        }
        assert!(rx.try_recv().is_err());
        outbound.dispatch_response(
            first["id"].as_str().unwrap(),
            Some(json!({"estimated_cost_usd":1.0,"status":"estimated","reasons":[]})),
            None,
        );
        settle().await;
        view.tick(Some("a"), true, &rpc);
        assert_eq!(view.summary.as_ref().unwrap().estimated_cost_usd, Some(1.0));
        view.tick(Some("a"), false, &rpc);
        let second = request(&mut rx).await;
        outbound.dispatch_response(
            second["id"].as_str().unwrap(),
            Some(json!({"estimated_cost_usd":2.0,"status":"estimated","reasons":[]})),
            None,
        );
        settle().await;
        view.tick(Some("a"), false, &rpc);
        tokio::time::advance(Duration::from_secs(2)).await;
        view.tick(Some("a"), false, &rpc);
        assert_eq!(request(&mut rx).await["params"]["session_id"], "a");
        view.tick(None, false, &rpc);
        assert!(view.summary.is_none());
        tokio::time::advance(Duration::from_secs(30)).await;
        view.tick(None, false, &rpc);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn conversation_cost_switch_discards_old_result_and_denial_stops_polling() {
        let (rpc, outbound, mut rx) = client();
        let mut view = ConversationCost::default();
        view.tick(Some("a"), false, &rpc);
        let old = request(&mut rx).await;
        view.tick(Some("b"), false, &rpc);
        let current = request(&mut rx).await;
        outbound.dispatch_response(
            old["id"].as_str().unwrap(),
            Some(json!({"estimated_cost_usd":99.0,"status":"estimated","reasons":[]})),
            None,
        );
        outbound.dispatch_response(
            current["id"].as_str().unwrap(),
            None,
            Some(JsonRpcError {
                code: zeroclaw_api::jsonrpc::error_codes::FORBIDDEN,
                message: "denied".into(),
                data: None,
            }),
        );
        settle().await;
        view.tick(Some("b"), false, &rpc);
        assert!(view.summary.is_none());
        tokio::time::advance(Duration::from_secs(60)).await;
        view.tick(Some("b"), false, &rpc);
        assert!(rx.try_recv().is_err());
        // Reconnection rebuilds the pane and therefore its stopped view.
        let mut reconnected = ConversationCost::default();
        reconnected.tick(Some("b"), false, &rpc);
        assert_eq!(request(&mut rx).await["params"]["session_id"], "b");
    }

    #[tokio::test(start_paused = true)]
    async fn conversation_cost_failures_back_off_and_old_servers_stop() {
        let (rpc, outbound, mut rx) = client();
        let mut view = ConversationCost::default();
        for delay in [5, 10, 20, 30, 30] {
            view.tick(Some("a"), false, &rpc);
            let request = request(&mut rx).await;
            outbound.dispatch_response(
                request["id"].as_str().unwrap(),
                None,
                Some(JsonRpcError {
                    code: -32603,
                    message: "disk unavailable".into(),
                    data: None,
                }),
            );
            settle().await;
            view.tick(Some("a"), false, &rpc);
            assert_eq!(
                view.next_poll.unwrap() - Instant::now(),
                Duration::from_secs(delay)
            );
            assert!(view.summary.is_none());
            tokio::time::advance(Duration::from_secs(delay)).await;
        }
        view.tick(Some("a"), false, &rpc);
        let request = request(&mut rx).await;
        outbound.dispatch_response(
            request["id"].as_str().unwrap(),
            None,
            Some(JsonRpcError {
                code: -32601,
                message: "unknown method".into(),
                data: None,
            }),
        );
        settle().await;
        view.tick(Some("a"), false, &rpc);
        view.tick(Some("b"), false, &rpc);
        assert!(view.unsupported);
        assert!(rx.try_recv().is_err());
    }
}
