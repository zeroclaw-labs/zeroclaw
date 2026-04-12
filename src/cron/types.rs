use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::LazyLock;

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

static SCHEDULE_SCHEMA: LazyLock<serde_json::Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "description": "When to run. Set 'kind' plus the fields for that kind.\n\
            kind='cron': set 'expr' (required), optionally 'tz'.\n\
            kind='at': set 'at' (required).\n\
            kind='every': set 'every_ms' (required).",
        "properties": {
            "kind": {
                "type": "string",
                "enum": ["cron", "at", "every"],
                "description": "Schedule type: 'cron' for repeating cron expression, 'at' for one-shot datetime, 'every' for repeating interval"
            },
            "expr": {
                "type": "string",
                "description": "Standard 5-field cron expression, e.g. '*/5 * * * *' or '0 9 * * 1-5'. Required when kind='cron'."
            },
            "tz": {
                "type": "string",
                "description": "IANA timezone name, e.g. 'America/New_York'. Defaults to UTC. Only used when kind='cron'."
            },
            "at": {
                "type": "string",
                "description": "ISO 8601 / RFC 3339 datetime, e.g. '2025-12-31T23:59:00Z' or '2025-12-31T23:59:00-05:00'. Timezone offsets are accepted and converted to UTC. Required when kind='at'."
            },
            "every_ms": {
                "type": "integer",
                "description": "Interval in milliseconds. Common values: 60000 (1 min), 300000 (5 min), 3600000 (1 hour), 86400000 (1 day). Required when kind='every'."
            }
        },
        "required": ["kind"]
    })
});

static DELIVERY_SCHEMA: LazyLock<serde_json::Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "description": "Optional delivery config to send job output to a channel after each run.",
        "properties": {
            "mode": {
                "type": "string",
                "enum": ["none", "announce"],
                "description": "'announce' sends output to the specified channel; 'none' disables delivery"
            },
            "channel": {
                "type": "string",
                "enum": ["telegram", "discord", "slack", "mattermost", "matrix", "qq"],
                "description": "Channel type to deliver output to"
            },
            "to": {
                "type": "string",
                "description": "Destination ID: Discord channel ID, Telegram chat ID, Slack channel name, etc."
            },
            "best_effort": {
                "type": "boolean",
                "description": "If true, a delivery failure does not fail the job itself. Defaults to true."
            }
        }
    })
});

/// Flat-object JSON Schema for the `Schedule` type.
///
/// Uses a single object with a `kind` discriminator instead of `oneOf`,
/// which is more reliable for LLM tool-calling across providers.
pub fn schedule_json_schema() -> serde_json::Value {
    SCHEDULE_SCHEMA.clone()
}

/// JSON Schema for the delivery config, shared by cron_add and cron_update.
pub fn delivery_json_schema() -> serde_json::Value {
    DELIVERY_SCHEMA.clone()
}

/// Format a human- and LLM-readable error for failed Schedule parsing.
pub fn format_schedule_error(serde_err: &serde_json::Error) -> String {
    format!(
        "Invalid schedule: {serde_err}. Expected object with \"kind\" set to \"cron\", \"at\", or \"every\":\n\
         - kind=\"cron\": requires \"expr\" (e.g. \"*/5 * * * *\"), optional \"tz\"\n\
         - kind=\"at\": requires \"at\" (ISO 8601 datetime, e.g. \"2025-12-31T23:59:00Z\")\n\
         - kind=\"every\": requires \"every_ms\" (interval in ms, e.g. 3600000 for 1 hour)"
    )
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
