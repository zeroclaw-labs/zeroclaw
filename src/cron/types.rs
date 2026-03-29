use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Try to deserialize a `serde_json::Value` as `T`.  If the value is a JSON
/// string that looks like an object (i.e. the LLM double-serialized it), parse
/// the inner string first and then deserialize the resulting object.  This
/// provides backward-compatible handling for both `Value::Object` and
/// `Value::String` representations.
pub fn deserialize_maybe_stringified<T: serde::de::DeserializeOwned>(
    v: &serde_json::Value,
) -> Result<T, serde_json::Error> {
    // Fast path: value is already the right shape (object, array, etc.)
    match serde_json::from_value::<T>(v.clone()) {
        Ok(parsed) => Ok(parsed),
        Err(first_err) => {
            // If it's a string, try parsing the string as JSON first.
            if let Some(s) = v.as_str() {
                let s = s.trim();
                if s.starts_with('{') || s.starts_with('[') {
                    if let Ok(inner) = serde_json::from_str::<serde_json::Value>(s) {
                        return serde_json::from_value::<T>(inner);
                    }
                }
            }
            Err(first_err)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum JobType {
    #[default]
    Shell,
    Agent,
}

impl From<JobType> for &'static str {
    fn from(value: JobType) -> Self {
        match value {
            JobType::Shell => "shell",
            JobType::Agent => "agent",
        }
    }
}

impl TryFrom<&str> for JobType {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value.to_lowercase().as_str() {
            "shell" => Ok(JobType::Shell),
            "agent" => Ok(JobType::Agent),
            _ => Err(format!(
                "Invalid job type '{}'. Expected one of: 'shell', 'agent'",
                value
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SessionTarget {
    #[default]
    Isolated,
    Main,
}

impl SessionTarget {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Isolated => "isolated",
            Self::Main => "main",
        }
    }

    pub(crate) fn parse(raw: &str) -> Self {
        if raw.eq_ignore_ascii_case("main") {
            Self::Main
        } else {
            Self::Isolated
        }
    }
}

/// Condition type for conditional schedules.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConditionType {
    /// Active during a date/time range.
    DateRange,
}

impl FromStr for ConditionType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "date_range" => Ok(Self::DateRange),
            other => Err(format!(
                "Invalid condition type '{other}'. Expected one of: 'date_range'"
            )),
        }
    }
}

/// A conditional schedule that overrides the primary schedule when its
/// condition is active.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConditionalSchedule {
    /// The more frequent schedule to use when the condition is active.
    pub schedule: Schedule,
    /// The type of condition that activates this schedule.
    pub condition_type: ConditionType,
    /// Start of the condition window (inclusive).
    pub condition_start: DateTime<Utc>,
    /// End of the condition window (inclusive).
    pub condition_end: DateTime<Utc>,
}

impl ConditionalSchedule {
    /// Returns `true` if the condition is active at the given timestamp.
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        match self.condition_type {
            ConditionType::DateRange => now >= self.condition_start && now <= self.condition_end,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Schedule {
    Cron {
        expr: String,
        #[serde(default)]
        tz: Option<String>,
    },
    At {
        at: DateTime<Utc>,
    },
    Every {
        every_ms: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryConfig {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default = "default_true")]
    pub best_effort: bool,
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            mode: "none".to_string(),
            channel: None,
            to: None,
            best_effort: true,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_source() -> String {
    "imperative".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub expression: String,
    pub schedule: Schedule,
    pub command: String,
    pub prompt: Option<String>,
    pub name: Option<String>,
    pub job_type: JobType,
    pub session_target: SessionTarget,
    pub model: Option<String>,
    pub enabled: bool,
    pub delivery: DeliveryConfig,
    pub delete_after_run: bool,
    /// Optional allowlist of tool names this cron job may use.
    /// When `Some(list)`, only tools whose name is in the list are available.
    /// When `None`, all tools are available (backward compatible default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    /// Optional conditional schedule that overrides the primary schedule
    /// when its condition is active (e.g. more frequent polling near a deadline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditional_schedule: Option<ConditionalSchedule>,
    /// How the job was created: `"imperative"` (CLI/API) or `"declarative"` (config).
    #[serde(default = "default_source")]
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub next_run: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub last_output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronRun {
    pub id: i64,
    pub job_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub status: String,
    pub output: Option<String>,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CronJobPatch {
    pub schedule: Option<Schedule>,
    pub command: Option<String>,
    pub prompt: Option<String>,
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub delivery: Option<DeliveryConfig>,
    pub model: Option<String>,
    pub session_target: Option<SessionTarget>,
    pub delete_after_run: Option<bool>,
    pub allowed_tools: Option<Vec<String>>,
    pub conditional_schedule: Option<Option<ConditionalSchedule>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_schedule_from_object() {
        let val = serde_json::json!({"kind": "cron", "expr": "*/5 * * * *"});
        let sched = deserialize_maybe_stringified::<Schedule>(&val).unwrap();
        assert!(matches!(sched, Schedule::Cron { ref expr, .. } if expr == "*/5 * * * *"));
    }

    #[test]
    fn deserialize_schedule_from_string() {
        let val = serde_json::Value::String(r#"{"kind":"cron","expr":"*/5 * * * *"}"#.to_string());
        let sched = deserialize_maybe_stringified::<Schedule>(&val).unwrap();
        assert!(matches!(sched, Schedule::Cron { ref expr, .. } if expr == "*/5 * * * *"));
    }

    #[test]
    fn deserialize_schedule_string_with_tz() {
        let val = serde_json::Value::String(
            r#"{"kind":"cron","expr":"*/30 9-15 * * 1-5","tz":"Asia/Shanghai"}"#.to_string(),
        );
        let sched = deserialize_maybe_stringified::<Schedule>(&val).unwrap();
        match sched {
            Schedule::Cron { tz, .. } => assert_eq!(tz.as_deref(), Some("Asia/Shanghai")),
            _ => panic!("expected Cron variant"),
        }
    }

    #[test]
    fn deserialize_every_from_string() {
        let val = serde_json::Value::String(r#"{"kind":"every","every_ms":60000}"#.to_string());
        let sched = deserialize_maybe_stringified::<Schedule>(&val).unwrap();
        assert!(matches!(sched, Schedule::Every { every_ms: 60000 }));
    }

    #[test]
    fn deserialize_invalid_string_returns_error() {
        let val = serde_json::Value::String("not json at all".to_string());
        assert!(deserialize_maybe_stringified::<Schedule>(&val).is_err());
    }

    #[test]
    fn job_type_try_from_accepts_known_values_case_insensitive() {
        assert_eq!(JobType::try_from("shell").unwrap(), JobType::Shell);
        assert_eq!(JobType::try_from("SHELL").unwrap(), JobType::Shell);
        assert_eq!(JobType::try_from("agent").unwrap(), JobType::Agent);
        assert_eq!(JobType::try_from("AgEnT").unwrap(), JobType::Agent);
    }

    #[test]
    fn job_type_try_from_rejects_invalid_values() {
        assert!(JobType::try_from("").is_err());
        assert!(JobType::try_from("unknown").is_err());
    }
}
