use super::types::{CostRecord, UsageCompleteness};
use zeroclaw_api::cost::{
    ConversationCostReason as Reason, ConversationCostStatus, ConversationCostSummary,
};

/// Ephemeral fold of the ledger, never a second balance or retained index.
#[derive(Default)]
pub(super) struct ConversationSummary {
    cost_usd: f64,
    has_priced_usage: bool,
    has_records: bool,
    reasons: Vec<Reason>,
}

impl ConversationSummary {
    pub fn reason(&mut self, reason: Reason) {
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
    }

    pub fn record(&mut self, record: CostRecord) {
        self.has_records = true;
        let usage = &record.usage;
        if !usage.cost_usd.is_finite()
            || usage.cost_usd < 0.0
            || usage.input_tokens.checked_add(usage.output_tokens) != Some(usage.total_tokens)
            || usage.cached_input_tokens > usage.input_tokens
            || usage.cache_creation_input_tokens
                > usage.input_tokens.saturating_sub(usage.cached_input_tokens)
            || usage.unpriced_tokens > usage.total_tokens
        {
            self.reason(Reason::LedgerIntegrity);
            return;
        }
        match record.usage_completeness {
            Some(UsageCompleteness::Complete) => {}
            Some(UsageCompleteness::LowerBound) => self.reason(Reason::MissingUsage),
            Some(UsageCompleteness::Unavailable) => {
                if usage.cost_usd != 0.0 || usage.total_tokens != 0 {
                    self.reason(Reason::LedgerIntegrity);
                }
                self.reason(Reason::MissingUsage);
                return;
            }
            None => self.reason(Reason::UnverifiedHistory),
        }
        let unpriced = if usage.unpriced_tokens > 0 {
            usage.unpriced_tokens
        } else if !usage.pricing_available {
            usage.total_tokens
        } else {
            0
        };
        if unpriced > 0 || !usage.pricing_available {
            self.reason(Reason::MissingPrices);
        }
        self.has_priced_usage |= usage.cost_usd > 0.0
            || usage.total_tokens > unpriced
            || (usage.total_tokens == 0
                && usage.pricing_available
                && record.usage_completeness == Some(UsageCompleteness::Complete));
        self.cost_usd += usage.cost_usd;
    }

    pub fn finish(mut self) -> ConversationCostSummary {
        if !self.has_records {
            self.reason(Reason::NoRecordedUsage);
        }
        if !self.cost_usd.is_finite() {
            return ConversationCostSummary::unavailable(Reason::LedgerIntegrity);
        }
        ConversationCostSummary {
            estimated_cost_usd: self.has_priced_usage.then_some(self.cost_usd),
            status: if !self.has_priced_usage {
                ConversationCostStatus::Unavailable
            } else if self.reasons.is_empty() {
                ConversationCostStatus::Estimated
            } else {
                ConversationCostStatus::Partial
            },
            reasons: self.reasons,
        }
    }
}
