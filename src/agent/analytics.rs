//! Conversation analytics for conversational AI agent workflows.
//!
//! Tracks per-conversation metrics and provides aggregation for reporting.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Per-conversation tracking record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationRecord {
    pub conversation_id: String,
    pub channel: String,
    pub language: String,
    /// Unix timestamp (seconds) when the conversation started.
    pub start_time: u64,
    /// Unix timestamp (seconds) when the conversation ended, if ended.
    pub end_time: Option<u64>,
    /// Number of turns in the conversation.
    pub turns: usize,
    /// Whether the conversation was resolved by the bot.
    pub resolved: bool,
    /// Whether the conversation was escalated to a human agent.
    pub escalated: bool,
    /// Intents detected during the conversation.
    pub intents: Vec<String>,
}

impl ConversationRecord {
    pub fn new(conversation_id: String, channel: String, language: String) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            conversation_id,
            channel,
            language,
            start_time: now,
            end_time: None,
            turns: 0,
            resolved: false,
            escalated: false,
            intents: Vec::new(),
        }
    }

    /// Mark the conversation as ended at the current time.
    pub fn mark_ended(&mut self) {
        self.end_time = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
    }

    /// Duration in seconds, if ended.
    pub fn duration_secs(&self) -> Option<u64> {
        self.end_time.map(|end| end.saturating_sub(self.start_time))
    }
}

/// Aggregated metrics over a collection of conversation records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMetrics {
    /// Total conversations in the reporting period.
    pub total_conversations: usize,
    /// Fraction of conversations that were resolved without escalation.
    pub resolution_rate: f64,
    /// Average number of turns per conversation.
    pub avg_turns: f64,
    /// Distribution of detected intents (intent name -> count).
    pub intent_distribution: HashMap<String, usize>,
    /// Distribution of languages (language -> count).
    pub language_distribution: HashMap<String, usize>,
    /// Fraction of conversations that were escalated.
    pub escalation_rate: f64,
    /// Average conversation duration in seconds (only for ended conversations).
    pub avg_duration_secs: Option<f64>,
}

/// Compute aggregated metrics from a slice of conversation records.
pub fn aggregate_metrics(records: &[ConversationRecord]) -> ConversationMetrics {
    let total = records.len();
    if total == 0 {
        return ConversationMetrics {
            total_conversations: 0,
            resolution_rate: 0.0,
            avg_turns: 0.0,
            intent_distribution: HashMap::new(),
            language_distribution: HashMap::new(),
            escalation_rate: 0.0,
            avg_duration_secs: None,
        };
    }

    let resolved_count = records.iter().filter(|r| r.resolved).count();
    let escalated_count = records.iter().filter(|r| r.escalated).count();
    let total_turns: usize = records.iter().map(|r| r.turns).sum();

    let mut intent_distribution: HashMap<String, usize> = HashMap::new();
    let mut language_distribution: HashMap<String, usize> = HashMap::new();

    for record in records {
        *language_distribution
            .entry(record.language.clone())
            .or_default() += 1;
        for intent in &record.intents {
            *intent_distribution.entry(intent.clone()).or_default() += 1;
        }
    }

    let durations: Vec<u64> = records.iter().filter_map(|r| r.duration_secs()).collect();
    let avg_duration_secs = if durations.is_empty() {
        None
    } else {
        let sum: u64 = durations.iter().sum();
        Some(sum as f64 / durations.len() as f64)
    };

    ConversationMetrics {
        total_conversations: total,
        resolution_rate: resolved_count as f64 / total as f64,
        avg_turns: total_turns as f64 / total as f64,
        intent_distribution,
        language_distribution,
        escalation_rate: escalated_count as f64 / total as f64,
        avg_duration_secs,
    }
}

/// Filter records by a time range (start_time within [from, to]).
pub fn filter_by_period(
    records: &[ConversationRecord],
    from_unix: u64,
    to_unix: u64,
) -> Vec<&ConversationRecord> {
    records
        .iter()
        .filter(|r| r.start_time >= from_unix && r.start_time <= to_unix)
        .collect()
}

/// Export metrics as a JSON string for dashboard consumption.
pub fn export_metrics_json(metrics: &ConversationMetrics) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(metrics)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(
        resolved: bool,
        escalated: bool,
        turns: usize,
        lang: &str,
    ) -> ConversationRecord {
        ConversationRecord {
            conversation_id: format!("conv-{turns}"),
            channel: "telegram".into(),
            language: lang.into(),
            start_time: 1000,
            end_time: Some(1000 + (turns as u64) * 10),
            turns,
            resolved,
            escalated,
            intents: vec!["greeting".into()],
        }
    }

    #[test]
    fn aggregate_metrics_empty() {
        let metrics = aggregate_metrics(&[]);
        assert_eq!(metrics.total_conversations, 0);
        assert_eq!(metrics.resolution_rate, 0.0);
        assert!(metrics.avg_duration_secs.is_none());
    }

    #[test]
    fn aggregate_metrics_single_resolved() {
        let records = vec![make_record(true, false, 5, "en")];
        let metrics = aggregate_metrics(&records);
        assert_eq!(metrics.total_conversations, 1);
        assert!((metrics.resolution_rate - 1.0).abs() < f64::EPSILON);
        assert!((metrics.escalation_rate - 0.0).abs() < f64::EPSILON);
        assert!((metrics.avg_turns - 5.0).abs() < f64::EPSILON);
        assert_eq!(*metrics.language_distribution.get("en").unwrap(), 1);
    }

    #[test]
    fn aggregate_metrics_mixed() {
        let records = vec![
            make_record(true, false, 3, "en"),
            make_record(false, true, 8, "de"),
            make_record(true, false, 2, "en"),
            make_record(false, false, 10, "fr"),
        ];
        let metrics = aggregate_metrics(&records);
        assert_eq!(metrics.total_conversations, 4);
        assert!((metrics.resolution_rate - 0.5).abs() < f64::EPSILON);
        assert!((metrics.escalation_rate - 0.25).abs() < f64::EPSILON);
        assert!((metrics.avg_turns - 5.75).abs() < f64::EPSILON);
        assert_eq!(*metrics.language_distribution.get("en").unwrap(), 2);
        assert_eq!(*metrics.language_distribution.get("de").unwrap(), 1);
    }

    #[test]
    fn aggregate_metrics_intent_distribution() {
        let mut record = make_record(true, false, 3, "en");
        record.intents = vec!["greeting".into(), "billing".into(), "greeting".into()];
        let metrics = aggregate_metrics(&[record]);
        assert_eq!(*metrics.intent_distribution.get("greeting").unwrap(), 2);
        assert_eq!(*metrics.intent_distribution.get("billing").unwrap(), 1);
    }

    #[test]
    fn conversation_record_duration() {
        let mut record = ConversationRecord::new("conv-1".into(), "slack".into(), "en".into());
        assert!(record.duration_secs().is_none());
        record.end_time = Some(record.start_time + 120);
        assert_eq!(record.duration_secs(), Some(120));
    }

    #[test]
    fn filter_by_period_filters_correctly() {
        let records = vec![
            ConversationRecord {
                conversation_id: "a".into(),
                channel: "telegram".into(),
                language: "en".into(),
                start_time: 100,
                end_time: None,
                turns: 1,
                resolved: false,
                escalated: false,
                intents: vec![],
            },
            ConversationRecord {
                conversation_id: "b".into(),
                channel: "telegram".into(),
                language: "en".into(),
                start_time: 200,
                end_time: None,
                turns: 2,
                resolved: false,
                escalated: false,
                intents: vec![],
            },
            ConversationRecord {
                conversation_id: "c".into(),
                channel: "telegram".into(),
                language: "en".into(),
                start_time: 300,
                end_time: None,
                turns: 3,
                resolved: false,
                escalated: false,
                intents: vec![],
            },
        ];
        let filtered = filter_by_period(&records, 150, 250);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].conversation_id, "b");
    }

    #[test]
    fn export_metrics_json_produces_valid_json() {
        let metrics = aggregate_metrics(&[make_record(true, false, 5, "en")]);
        let json = export_metrics_json(&metrics).unwrap();
        assert!(json.contains("\"total_conversations\": 1"));
        assert!(json.contains("\"resolution_rate\""));
    }
}
