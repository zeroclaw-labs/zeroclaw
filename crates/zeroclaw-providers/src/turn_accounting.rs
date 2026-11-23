//! Shared per-turn usage accounting, context-window limit resolution,
//! post-trim display projection, and safeguard-fallback display projection.
//!
//! Used across runtime RPC and gateway chat surfaces so token totals, cost
//! attribution, context-meter display semantics, and safeguard-fallback
//! privacy guarantees share a single source of truth.

use crate::{SafeguardFallbackKind, SafeguardFallbackNotice};
use std::collections::HashMap;
use zeroclaw_api::agent::{TokenCountSource, TurnEvent};
use zeroclaw_config::schema::{
    ModelContextWindowSource, ResolvedContextLimits, UNCONFIGURED_CONTEXT_WINDOW_FALLBACK,
};

/// Per-provider usage snapshot in a turn's `usage_by_provider` breakdown.
/// Tracks ALL billable attempts (accepted + rejected Reliable attempts).
/// The scalar `cost_usd` is the sum of `usage_by_provider[*].cost_usd`,
/// making the breakdown the single source of truth for both token counts and cost.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderUsageEntry {
    pub provider_ref: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub cost_usd: f64,
}

/// Fold state for [`TurnEvent::Usage`] across one agent turn.
///
/// Billing aggregation (turn-wide totals plus the per-`(provider, model)`
/// breakdown) accumulates every billable attempt, including rejected ones.
/// The accepted-serving snapshot (`last_provider_ref` / `last_model` /
/// `last_input_tokens` / `last_context_token_budget` / `last_model_context_window`)
/// advances only on `accepted: true` events, so a later billed rejected attempt
/// cannot re-point the terminal identity or the context-meter ceiling.
#[derive(Debug, Default)]
pub struct UsageFold {
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    pub last_provider_ref: Option<String>,
    pub last_model: Option<String>,
    pub last_input_tokens: Option<u64>,
    pub last_context_token_budget: Option<u64>,
    pub last_model_context_window: Option<u64>,
    usage_by_provider: HashMap<(String, String), ProviderUsageEntry>,
    saw_usage: bool,
}

impl UsageFold {
    /// Fold a single [`TurnEvent`]; non-`Usage` variants are ignored.
    pub fn apply_ref(&mut self, event: &TurnEvent) {
        let TurnEvent::Usage {
            input_tokens,
            cached_input_tokens,
            output_tokens,
            cost_usd,
            context_token_budget,
            model_context_window,
            provider_ref,
            model: served_model,
            accepted,
            // Deliberately ignored: this fold is the provider-measured billing
            // ledger. Display estimates travel on the context-meter surface.
            estimated_input_tokens: _,
        } = event
        else {
            return;
        };

        self.saw_usage = true;
        if let Some(it) = *input_tokens {
            self.total_input_tokens = Some(self.total_input_tokens.unwrap_or(0).saturating_add(it));
        }
        if let Some(ot) = *output_tokens {
            self.total_output_tokens =
                Some(self.total_output_tokens.unwrap_or(0).saturating_add(ot));
        }
        if *accepted {
            self.last_provider_ref = Some(provider_ref.clone());
            self.last_model = Some(served_model.clone());
            self.last_context_token_budget = *context_token_budget;
            self.last_model_context_window = *model_context_window;
            // Clear the previous route's input snapshot when an accepted call
            // returns no usage data so stale counts are not paired with the
            // new route's context window.
            self.last_input_tokens = *input_tokens;
        }

        let entry = self
            .usage_by_provider
            .entry((provider_ref.clone(), served_model.clone()))
            .or_insert_with_key(|(provider_ref, model)| ProviderUsageEntry {
                provider_ref: provider_ref.clone(),
                model: model.clone(),
                ..Default::default()
            });
        if let Some(it) = *input_tokens {
            entry.input_tokens = entry.input_tokens.saturating_add(it);
        }
        if let Some(ot) = *output_tokens {
            entry.output_tokens = entry.output_tokens.saturating_add(ot);
        }
        if let Some(ct) = *cached_input_tokens {
            entry.cached_input_tokens = entry.cached_input_tokens.saturating_add(ct);
        }
        if let Some(cu) = *cost_usd {
            entry.cost_usd += cu;
        }
    }

    /// Convenience wrapper consuming an owned [`TurnEvent`].
    pub fn apply(&mut self, event: TurnEvent) {
        self.apply_ref(&event);
    }

    /// Whether at least one [`TurnEvent::Usage`] event was observed.
    pub fn saw_usage(&self) -> bool {
        self.saw_usage
    }

    /// Combined turn input + output tokens when at least one side was measured.
    pub fn total_tokens(&self) -> Option<u64> {
        match (self.total_input_tokens, self.total_output_tokens) {
            (None, None) => None,
            (i, o) => Some(i.unwrap_or(0).saturating_add(o.unwrap_or(0))),
        }
    }

    /// Non-destructive sorted per-`(provider_ref, model)` breakdown in
    /// deterministic wire order.
    pub fn sorted_entries(&self) -> Vec<ProviderUsageEntry> {
        let mut entries: Vec<_> = self.usage_by_provider.values().cloned().collect();
        entries.sort_by(|a, b| {
            a.provider_ref
                .cmp(&b.provider_ref)
                .then(a.model.cmp(&b.model))
        });
        entries
    }

    /// Sorted per-`(provider_ref, model)` breakdown in deterministic wire order;
    /// drains the internal map. `total_cost_usd` sums over this vector so float
    /// accumulation order is deterministic.
    pub fn take_sorted_entries(&mut self) -> Vec<ProviderUsageEntry> {
        let mut entries: Vec<_> = std::mem::take(&mut self.usage_by_provider)
            .into_values()
            .collect();
        entries.sort_by(|a, b| {
            a.provider_ref
                .cmp(&b.provider_ref)
                .then(a.model.cmp(&b.model))
        });
        entries
    }

    /// Turn-wide cost total over the sorted breakdown; `None` when nothing
    /// billable was recorded.
    pub fn total_cost_usd(entries: &[ProviderUsageEntry]) -> Option<f64> {
        let sum: f64 = entries.iter().map(|e| e.cost_usd).sum();
        (sum > 0.0).then_some(sum)
    }
}

/// A per-turn display projection of the latest real history trim.
///
/// The trim event is authoritative for this retained-context estimate;
/// [`UsageFold`] remains the provider-measured ledger. Any later accepted call
/// (even one without usage) supersedes the estimate. Rejected billable attempts
/// do not.
#[derive(Debug, Default, Clone)]
pub struct ContextDisplay {
    trimmed_input: Option<(u64, Option<&'static str>)>,
}

impl ContextDisplay {
    pub fn observe(&mut self, event: &TurnEvent) {
        match event {
            TurnEvent::Usage { accepted: true, .. } => self.accepted_call(),
            TurnEvent::HistoryTrimmed {
                dropped_messages,
                tokens_after,
                tokens_after_source,
                ..
            } => self.trimmed(*dropped_messages, *tokens_after, *tokens_after_source),
            _ => {}
        }
    }

    /// A later accepted call measured the context again.
    pub fn accepted_call(&mut self) {
        self.trimmed_input = None;
    }

    /// Record a trim notice. Only one that dropped history and reports what was
    /// kept moves the displayed count: a zero-drop floor notice describes a
    /// rejected request, and a message-limit trim has no token count.
    pub fn trimmed(
        &mut self,
        dropped_messages: usize,
        tokens_after: Option<u64>,
        tokens_after_source: Option<TokenCountSource>,
    ) {
        if dropped_messages == 0 {
            return;
        }
        if let Some(after) = tokens_after {
            self.trimmed_input = Some((after, tokens_after_source.map(|source| source.as_str())));
        }
    }

    /// Retained context estimate after the most recent effective trim, if not
    /// superseded by a subsequent accepted model call.
    pub fn trimmed_input(&self) -> Option<(u64, Option<&'static str>)> {
        self.trimmed_input
    }
}

/// Resolve terminal budget and configured context window from usage values with
/// a fallback to active route limits.
pub fn resolve_done_context_limits(
    usage_budget: Option<u64>,
    usage_model_window: Option<u64>,
    active_limits: ResolvedContextLimits,
) -> (u64, Option<u64>) {
    (
        usage_budget.unwrap_or(active_limits.context_token_budget as u64),
        usage_model_window.or_else(|| {
            active_limits
                .configured_model_context_window()
                .map(|tokens| tokens as u64)
        }),
    )
}

/// Terminal-frame budget/window for turn completion telemetry.
///
/// `final_limits` is the route that actually served the LAST call, carried out
/// of the turn loop. When present it is AUTHORITATIVE and overrides the
/// usage-derived values, which can be stale when an earlier usage-bearing route
/// precedes a final no-usage call. Only when no call was served this turn (a
/// cache hit) does `final_limits` become `None`, and this falls back to usage
/// values, then to `fallback_limits`.
pub fn done_frame_context_limits(
    final_limits: Option<ResolvedContextLimits>,
    usage_budget: Option<u64>,
    usage_model_window: Option<u64>,
    fallback_limits: Option<ResolvedContextLimits>,
) -> (u64, Option<u64>) {
    match final_limits {
        Some(limits) => (
            limits.context_token_budget as u64,
            limits.configured_model_context_window().map(|t| t as u64),
        ),
        None => resolve_done_context_limits(
            usage_budget,
            usage_model_window,
            fallback_limits.unwrap_or(ResolvedContextLimits {
                model_context_window: UNCONFIGURED_CONTEXT_WINDOW_FALLBACK,
                context_token_budget: 0,
                model_context_window_source: ModelContextWindowSource::CompatibilityFallback,
            }),
        ),
    }
}

/// Wire-safe layer classification for [`SafeguardFallbackDisplay`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeguardFallbackKindLabel {
    Server,
    Client,
    ClientServer,
}

impl SafeguardFallbackKindLabel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Client => "client",
            Self::ClientServer => "client_server",
        }
    }
}

/// Wire-safe projection of a [`SafeguardFallbackNotice`].
///
/// Privacy contract: only the requested/served model names and the fallback
/// layer (`"server"` / `"client"` / `"client_server"`) cross the wire. The
/// classifier `category` and any refusal explanation are logs-only and MUST
/// NEVER reach clients — this helper deliberately never reads
/// `notice.category`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeguardFallbackDisplay {
    pub fallback_kind: SafeguardFallbackKindLabel,
    pub requested_model: String,
    pub served_model: String,
}

impl SafeguardFallbackDisplay {
    pub fn from_notice_ref(notice: &SafeguardFallbackNotice) -> Self {
        let fallback_kind = match notice.kind {
            SafeguardFallbackKind::ServerSide => SafeguardFallbackKindLabel::Server,
            SafeguardFallbackKind::ClientSide => SafeguardFallbackKindLabel::Client,
            SafeguardFallbackKind::ClientAndServer => SafeguardFallbackKindLabel::ClientServer,
        };
        Self {
            fallback_kind,
            requested_model: notice.requested_model.clone(),
            served_model: notice.served_model.clone(),
        }
    }

    pub fn from_notice(notice: Option<&SafeguardFallbackNotice>) -> Option<Self> {
        notice.map(Self::from_notice_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage_event(
        provider_ref: &str,
        model: &str,
        input_tokens: Option<u64>,
        cached_input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        cost_usd: Option<f64>,
        accepted: bool,
    ) -> TurnEvent {
        TurnEvent::Usage {
            input_tokens,
            cached_input_tokens,
            output_tokens,
            cost_usd,
            context_token_budget: Some(1_000),
            model_context_window: Some(4_000),
            provider_ref: provider_ref.to_string(),
            model: model.to_string(),
            accepted,
            estimated_input_tokens: Some(9_999),
        }
    }

    fn trim_event(dropped_messages: usize, tokens_after: Option<u64>) -> TurnEvent {
        TurnEvent::HistoryTrimmed {
            dropped_messages,
            dropped_turns: usize::from(dropped_messages > 0),
            kept_turns: 1,
            reason: "context token budget exceeded".into(),
            token_budget: Some(128_000),
            tokens_before: Some(180_000),
            tokens_after,
            tokens_before_source: Some(TokenCountSource::Provider),
            tokens_after_source: Some(TokenCountSource::Calibrated),
            unsatisfiable_floor: Some(false),
            retained_context: None,
        }
    }

    #[test]
    fn usage_fold_bills_all_attempts_keeps_accepted_snapshot_and_clears_on_usageless() {
        let mut fold = UsageFold::default();
        assert!(!fold.saw_usage());
        assert_eq!(fold.total_tokens(), None);

        fold.apply_ref(&TurnEvent::Chunk {
            delta: "ignored".into(),
        });
        assert!(!fold.saw_usage());

        fold.apply(usage_event(
            "b.rejected",
            "m-rej",
            Some(200),
            Some(50),
            Some(20),
            Some(0.25),
            false,
        ));
        assert!(fold.saw_usage());
        assert_eq!(fold.last_provider_ref, None);
        assert_eq!(fold.last_model, None);
        assert_eq!(fold.last_input_tokens, None);

        fold.apply(usage_event(
            "a.primary",
            "m1",
            Some(100),
            None,
            Some(40),
            Some(0.10),
            true,
        ));
        assert_eq!(fold.last_provider_ref.as_deref(), Some("a.primary"));
        assert_eq!(fold.last_model.as_deref(), Some("m1"));
        assert_eq!(fold.last_input_tokens, Some(100));
        assert_eq!(fold.last_context_token_budget, Some(1_000));
        assert_eq!(fold.last_model_context_window, Some(4_000));

        // Rejected attempt after accepted call must not re-point snapshot.
        fold.apply(usage_event(
            "c.rejected",
            "m-rej2",
            None,
            None,
            Some(5),
            Some(0.05),
            false,
        ));
        assert_eq!(fold.last_provider_ref.as_deref(), Some("a.primary"));
        assert_eq!(fold.last_input_tokens, Some(100));

        // Accepted usage-less call moves serving identity and clears last_input_tokens.
        fold.apply(usage_event(
            "ollama.fallback",
            "m-local",
            None,
            None,
            Some(15),
            None,
            true,
        ));
        assert_eq!(fold.last_provider_ref.as_deref(), Some("ollama.fallback"));
        assert_eq!(fold.last_model.as_deref(), Some("m-local"));
        assert_eq!(fold.last_input_tokens, None);

        assert_eq!(fold.total_input_tokens, Some(300));
        assert_eq!(fold.total_output_tokens, Some(80));
        assert_eq!(fold.total_tokens(), Some(380));

        let non_destructive = fold.sorted_entries();
        let drained = fold.take_sorted_entries();
        assert_eq!(non_destructive, drained);
        assert!(fold.sorted_entries().is_empty());

        let order: Vec<_> = drained.iter().map(|e| e.provider_ref.as_str()).collect();
        assert_eq!(
            order,
            ["a.primary", "b.rejected", "c.rejected", "ollama.fallback"]
        );
        let total_cost = UsageFold::total_cost_usd(&drained).expect("cost sum");
        assert!((total_cost - 0.40).abs() < 1e-12);
    }

    #[test]
    fn unpriced_usage_has_no_total_cost() {
        assert_eq!(UsageFold::total_cost_usd(&[]), None);
        let mut fold = UsageFold::default();
        fold.apply(usage_event(
            "local",
            "m",
            Some(12),
            None,
            Some(3),
            None,
            true,
        ));
        let entries = fold.sorted_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(UsageFold::total_cost_usd(&entries), None);
    }

    #[test]
    fn context_display_tracks_effective_trims_and_clears_on_accepted_usage() {
        let mut display = ContextDisplay::default();
        display.observe(&trim_event(0, Some(80_000)));
        display.observe(&trim_event(2, None));
        assert_eq!(display.trimmed_input(), None);

        display.observe(&trim_event(3, Some(65_000)));
        assert_eq!(display.trimmed_input(), Some((65_000, Some("calibrated"))));

        // Rejected usage preserves trim display.
        display.observe(&usage_event("r", "m", Some(500), None, None, None, false));
        assert_eq!(display.trimmed_input(), Some((65_000, Some("calibrated"))));

        // Accepted usage (even without input_tokens) supersedes trim display.
        display.observe(&usage_event("a", "m", None, None, None, None, true));
        assert_eq!(display.trimmed_input(), None);
    }

    #[test]
    fn done_frame_context_limits_respects_final_route_and_fallbacks() {
        let final_limits = ResolvedContextLimits {
            model_context_window: 8_000,
            context_token_budget: 7_200,
            model_context_window_source: ModelContextWindowSource::Configured,
        };
        assert_eq!(
            done_frame_context_limits(Some(final_limits), Some(180_000), Some(200_000), None),
            (7_200, Some(8_000)),
        );

        let fallback = ResolvedContextLimits {
            model_context_window: UNCONFIGURED_CONTEXT_WINDOW_FALLBACK,
            context_token_budget: 16_000,
            model_context_window_source: ModelContextWindowSource::CompatibilityFallback,
        };
        assert_eq!(
            resolve_done_context_limits(None, None, fallback),
            (16_000, None),
        );
        assert_eq!(
            done_frame_context_limits(None, Some(9_000), Some(12_000), Some(fallback)),
            (9_000, Some(12_000)),
        );
    }

    #[test]
    fn safeguard_fallback_display_maps_all_kinds_and_drops_category() {
        assert!(SafeguardFallbackDisplay::from_notice(None).is_none());

        for (kind, expected_label, expected_str) in [
            (
                SafeguardFallbackKind::ServerSide,
                SafeguardFallbackKindLabel::Server,
                "server",
            ),
            (
                SafeguardFallbackKind::ClientSide,
                SafeguardFallbackKindLabel::Client,
                "client",
            ),
            (
                SafeguardFallbackKind::ClientAndServer,
                SafeguardFallbackKindLabel::ClientServer,
                "client_server",
            ),
        ] {
            let notice = SafeguardFallbackNotice {
                kind,
                requested_model: "req-m".into(),
                served_model: "srv-m".into(),
                category: Some("sensitive-category".into()),
            };
            let wire = SafeguardFallbackDisplay::from_notice(Some(&notice)).expect("some notice");
            assert_eq!(wire.fallback_kind, expected_label);
            assert_eq!(wire.fallback_kind.as_str(), expected_str);
            assert_eq!(wire.requested_model, "req-m");
            assert_eq!(wire.served_model, "srv-m");
        }
    }
}
