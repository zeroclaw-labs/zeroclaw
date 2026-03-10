use crate::config::TelemetryConfig;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ── Event types ─────────────────────────────────────────────────

/// Severity level for flagged events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AlertLevel {
    /// Normal activity, no flag.
    None,
    /// Suspicious pattern detected (e.g. crypto/gambling domain).
    Warning,
    /// High-risk activity requiring immediate admin attention.
    Critical,
}

impl AlertLevel {
    pub fn as_str(&self) -> &str {
        match self {
            Self::None => "none",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "warning" => Self::Warning,
            "critical" => Self::Critical,
            _ => Self::None,
        }
    }
}

/// A single telemetry event recorded from a user session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEvent {
    /// Auto-generated event ID.
    #[serde(default)]
    pub id: i64,
    /// User identifier (from channel auth, pairing, or registration).
    pub user_id: String,
    /// ISO-3166 country code or IP-derived location (e.g. "KR", "US").
    #[serde(default)]
    pub country: String,
    /// Client IP address (for geo-lookup, not stored long-term if privacy required).
    #[serde(default)]
    pub ip_address: String,
    /// Channel the user is interacting through (e.g. "telegram", "discord", "web").
    #[serde(default)]
    pub channel: String,
    /// High-level action category.
    pub action: String,
    /// Target URL or app that the user's workflow interacted with.
    #[serde(default)]
    pub target_url: String,
    /// Detailed description of what the user did.
    #[serde(default)]
    pub details: String,
    /// Tool name used (e.g. "browser", "shell", "cron_add").
    #[serde(default)]
    pub tool_name: String,
    /// Alert level if suspicious activity was detected.
    #[serde(default = "default_alert_level")]
    pub alert_level: AlertLevel,
    /// Reason for alert (empty if alert_level is None).
    #[serde(default)]
    pub alert_reason: String,
    /// When this event occurred.
    #[serde(default = "Utc::now")]
    pub timestamp: DateTime<Utc>,
}

fn default_alert_level() -> AlertLevel {
    AlertLevel::None
}

/// Query parameters for searching telemetry events.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TelemetryQuery {
    /// Filter by user ID (exact match).
    pub user_id: Option<String>,
    /// Filter by country code.
    pub country: Option<String>,
    /// Filter by channel name.
    pub channel: Option<String>,
    /// Filter by action type.
    pub action: Option<String>,
    /// Filter by alert level.
    pub alert_level: Option<String>,
    /// Events after this timestamp.
    pub since: Option<DateTime<Utc>>,
    /// Events before this timestamp.
    pub until: Option<DateTime<Utc>>,
    /// Search in target_url or details.
    pub search: Option<String>,
    /// Maximum results to return (default 100).
    pub limit: Option<u32>,
    /// Offset for pagination.
    pub offset: Option<u32>,
}

/// Summary statistics for the admin dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySummary {
    pub total_events: i64,
    pub total_users: i64,
    pub total_alerts: i64,
    pub events_today: i64,
    pub top_countries: Vec<(String, i64)>,
    pub top_actions: Vec<(String, i64)>,
    pub recent_alerts: Vec<TelemetryEvent>,
}

// ── SQLite store ────────────────────────────────────────────────

pub struct TelemetryStore {
    conn: Mutex<Connection>,
    config: TelemetryConfig,
}

impl TelemetryStore {
    pub fn new(workspace_dir: &Path, config: TelemetryConfig) -> Result<Self> {
        let db_path = workspace_dir.join("telemetry").join("events.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create telemetry dir: {}", parent.display()))?;
        }

        let conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open telemetry DB: {}", db_path.display()))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA mmap_size    = 4194304;
             PRAGMA cache_size   = -1000;
             PRAGMA temp_store   = MEMORY;",
        )?;

        Self::init_schema(&conn)?;

        Ok(Self {
            conn: Mutex::new(conn),
            config,
        })
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS telemetry_events (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id      TEXT NOT NULL,
                country      TEXT NOT NULL DEFAULT '',
                ip_address   TEXT NOT NULL DEFAULT '',
                channel      TEXT NOT NULL DEFAULT '',
                action       TEXT NOT NULL,
                target_url   TEXT NOT NULL DEFAULT '',
                details      TEXT NOT NULL DEFAULT '',
                tool_name    TEXT NOT NULL DEFAULT '',
                alert_level  TEXT NOT NULL DEFAULT 'none',
                alert_reason TEXT NOT NULL DEFAULT '',
                timestamp    TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_telemetry_user_id ON telemetry_events(user_id);
            CREATE INDEX IF NOT EXISTS idx_telemetry_timestamp ON telemetry_events(timestamp);
            CREATE INDEX IF NOT EXISTS idx_telemetry_alert_level ON telemetry_events(alert_level);
            CREATE INDEX IF NOT EXISTS idx_telemetry_country ON telemetry_events(country);
            CREATE INDEX IF NOT EXISTS idx_telemetry_action ON telemetry_events(action);",
        )?;
        Ok(())
    }

    /// Classify an event for suspicious patterns and set alert level.
    pub fn classify_alert(&self, event: &mut TelemetryEvent) {
        if !self.config.alerts_enabled {
            return;
        }

        let url_lower = event.target_url.to_ascii_lowercase();
        let details_lower = event.details.to_ascii_lowercase();
        let combined = format!("{url_lower} {details_lower}");

        for pattern in &self.config.suspicious_patterns {
            let pat_lower = pattern.to_ascii_lowercase();
            if combined.contains(&pat_lower) {
                event.alert_level = AlertLevel::Warning;
                event.alert_reason = format!("Suspicious pattern matched: {pattern}");

                // Escalate to critical for financial transaction indicators
                if Self::is_financial_indicator(&combined) {
                    event.alert_level = AlertLevel::Critical;
                    event.alert_reason = format!(
                        "CRITICAL: Financial activity detected with suspicious target. Pattern: {pattern}"
                    );
                }
                return;
            }
        }
    }

    /// Check for financial transaction keywords.
    fn is_financial_indicator(text: &str) -> bool {
        const FINANCIAL_KEYWORDS: &[&str] = &[
            "withdraw", "deposit", "transfer", "trade", "buy", "sell", "profit", "earning",
            "revenue", "payment", "wallet", "bitcoin", "ethereum", "crypto", "출금", "입금",
            "송금", "거래", "매수", "매도", "수익", "배팅", "베팅", "도박",
        ];
        FINANCIAL_KEYWORDS.iter().any(|kw| text.contains(kw))
    }

    /// Record a telemetry event. Classifies alerts automatically.
    pub fn record(&self, mut event: TelemetryEvent) -> Result<()> {
        self.classify_alert(&mut event);

        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO telemetry_events
                (user_id, country, ip_address, channel, action, target_url,
                 details, tool_name, alert_level, alert_reason, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                event.user_id,
                event.country,
                event.ip_address,
                event.channel,
                event.action,
                event.target_url,
                event.details,
                event.tool_name,
                event.alert_level.as_str(),
                event.alert_reason,
                event.timestamp.to_rfc3339(),
            ],
        )?;

        // Prune old events beyond retention
        self.prune_if_needed(&conn)?;

        Ok(())
    }

    /// Query events with filters.
    pub fn query(&self, q: &TelemetryQuery) -> Result<Vec<TelemetryEvent>> {
        use std::fmt::Write;
        let conn = self.conn.lock();

        let mut sql = String::from(
            "SELECT id, user_id, country, ip_address, channel, action, target_url,
                    details, tool_name, alert_level, alert_reason, timestamp
             FROM telemetry_events WHERE 1=1",
        );
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut param_idx = 1;

        if let Some(ref user_id) = q.user_id {
            let _ = write!(sql, " AND user_id = ?{param_idx}");
            bind_values.push(Box::new(user_id.clone()));
            param_idx += 1;
        }
        if let Some(ref country) = q.country {
            let _ = write!(sql, " AND country = ?{param_idx}");
            bind_values.push(Box::new(country.clone()));
            param_idx += 1;
        }
        if let Some(ref channel) = q.channel {
            let _ = write!(sql, " AND channel = ?{param_idx}");
            bind_values.push(Box::new(channel.clone()));
            param_idx += 1;
        }
        if let Some(ref action) = q.action {
            let _ = write!(sql, " AND action = ?{param_idx}");
            bind_values.push(Box::new(action.clone()));
            param_idx += 1;
        }
        if let Some(ref alert_level) = q.alert_level {
            let _ = write!(sql, " AND alert_level = ?{param_idx}");
            bind_values.push(Box::new(alert_level.clone()));
            param_idx += 1;
        }
        if let Some(ref since) = q.since {
            let _ = write!(sql, " AND timestamp >= ?{param_idx}");
            bind_values.push(Box::new(since.to_rfc3339()));
            param_idx += 1;
        }
        if let Some(ref until) = q.until {
            let _ = write!(sql, " AND timestamp <= ?{param_idx}");
            bind_values.push(Box::new(until.to_rfc3339()));
            param_idx += 1;
        }
        if let Some(ref search) = q.search {
            let pattern = format!("%{search}%");
            let _ = write!(
                sql,
                " AND (target_url LIKE ?{param_idx} OR details LIKE ?{})",
                param_idx + 1
            );
            bind_values.push(Box::new(pattern.clone()));
            bind_values.push(Box::new(pattern));
            param_idx += 2;
        }

        sql.push_str(" ORDER BY timestamp DESC");

        let limit = q.limit.unwrap_or(100);
        let _ = write!(sql, " LIMIT ?{param_idx}");
        bind_values.push(Box::new(limit));
        param_idx += 1;

        if let Some(offset) = q.offset {
            let _ = write!(sql, " OFFSET ?{param_idx}");
            bind_values.push(Box::new(offset));
        }

        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_refs.as_slice(), |row| {
            Ok(TelemetryEvent {
                id: row.get(0)?,
                user_id: row.get(1)?,
                country: row.get(2)?,
                ip_address: row.get(3)?,
                channel: row.get(4)?,
                action: row.get(5)?,
                target_url: row.get(6)?,
                details: row.get(7)?,
                tool_name: row.get(8)?,
                alert_level: AlertLevel::from_str_lossy(
                    &row.get::<_, String>(9).unwrap_or_default(),
                ),
                alert_reason: row.get(10)?,
                timestamp: row
                    .get::<_, String>(11)
                    .ok()
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now),
            })
        })?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    /// Get summary statistics for admin dashboard.
    pub fn summary(&self) -> Result<TelemetrySummary> {
        let conn = self.conn.lock();

        let total_events: i64 =
            conn.query_row("SELECT COUNT(*) FROM telemetry_events", [], |row| {
                row.get(0)
            })?;

        let total_users: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT user_id) FROM telemetry_events",
            [],
            |row| row.get(0),
        )?;

        let total_alerts: i64 = conn.query_row(
            "SELECT COUNT(*) FROM telemetry_events WHERE alert_level != 'none'",
            [],
            |row| row.get(0),
        )?;

        let today = Utc::now().format("%Y-%m-%d").to_string();
        let events_today: i64 = conn.query_row(
            "SELECT COUNT(*) FROM telemetry_events WHERE timestamp >= ?1",
            params![format!("{today}T00:00:00Z")],
            |row| row.get(0),
        )?;

        let mut stmt = conn.prepare_cached(
            "SELECT country, COUNT(*) as cnt FROM telemetry_events
             WHERE country != '' GROUP BY country ORDER BY cnt DESC LIMIT 10",
        )?;
        let top_countries: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        let mut stmt = conn.prepare_cached(
            "SELECT action, COUNT(*) as cnt FROM telemetry_events
             GROUP BY action ORDER BY cnt DESC LIMIT 10",
        )?;
        let top_actions: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        // Recent alerts (last 20)
        let mut stmt = conn.prepare_cached(
            "SELECT id, user_id, country, ip_address, channel, action, target_url,
                    details, tool_name, alert_level, alert_reason, timestamp
             FROM telemetry_events WHERE alert_level != 'none'
             ORDER BY timestamp DESC LIMIT 20",
        )?;
        let recent_alerts: Vec<TelemetryEvent> = stmt
            .query_map([], |row| {
                Ok(TelemetryEvent {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    country: row.get(2)?,
                    ip_address: row.get(3)?,
                    channel: row.get(4)?,
                    action: row.get(5)?,
                    target_url: row.get(6)?,
                    details: row.get(7)?,
                    tool_name: row.get(8)?,
                    alert_level: AlertLevel::from_str_lossy(
                        &row.get::<_, String>(9).unwrap_or_default(),
                    ),
                    alert_reason: row.get(10)?,
                    timestamp: row
                        .get::<_, String>(11)
                        .ok()
                        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(Utc::now),
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(TelemetrySummary {
            total_events,
            total_users,
            total_alerts,
            events_today,
            top_countries,
            top_actions,
            recent_alerts,
        })
    }

    /// Get pending alerts that need admin notification.
    pub fn pending_alerts(&self) -> Result<Vec<TelemetryEvent>> {
        self.query(&TelemetryQuery {
            alert_level: Some("critical".into()),
            limit: Some(50),
            ..Default::default()
        })
    }

    fn prune_if_needed(&self, conn: &Connection) -> Result<()> {
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM telemetry_events", [], |row| {
            row.get(0)
        })?;

        let max_events = self.config.max_events as i64;
        if count > max_events {
            let excess = count - max_events;
            conn.execute(
                "DELETE FROM telemetry_events WHERE id IN (
                    SELECT id FROM telemetry_events ORDER BY timestamp ASC LIMIT ?1
                )",
                params![excess],
            )?;
        }

        // Prune by retention days
        let cutoff = Utc::now() - chrono::Duration::days(i64::from(self.config.retention_days));
        conn.execute(
            "DELETE FROM telemetry_events WHERE timestamp < ?1",
            params![cutoff.to_rfc3339()],
        )?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config() -> TelemetryConfig {
        TelemetryConfig {
            enabled: true,
            admin_token: Some("test-admin-token".into()),
            alerts_enabled: true,
            suspicious_patterns: vec!["binance.com".into(), "casino".into(), "gambling".into()],
            ..TelemetryConfig::default()
        }
    }

    fn test_store(tmp: &TempDir) -> TelemetryStore {
        TelemetryStore::new(tmp.path(), test_config()).unwrap()
    }

    fn make_event(user_id: &str, action: &str, target: &str) -> TelemetryEvent {
        TelemetryEvent {
            id: 0,
            user_id: user_id.into(),
            country: "KR".into(),
            ip_address: "1.2.3.4".into(),
            channel: "telegram".into(),
            action: action.into(),
            target_url: target.into(),
            details: String::new(),
            tool_name: "browser".into(),
            alert_level: AlertLevel::None,
            alert_reason: String::new(),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn record_and_query_events() {
        let tmp = TempDir::new().unwrap();
        let store = test_store(&tmp);

        store
            .record(make_event("user_a", "browse", "https://example.com"))
            .unwrap();
        store
            .record(make_event("user_b", "browse", "https://other.com"))
            .unwrap();

        let all = store.query(&TelemetryQuery::default()).unwrap();
        assert_eq!(all.len(), 2);

        let user_a_events = store
            .query(&TelemetryQuery {
                user_id: Some("user_a".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(user_a_events.len(), 1);
        assert_eq!(user_a_events[0].user_id, "user_a");
    }

    #[test]
    fn alert_classification_detects_crypto() {
        let tmp = TempDir::new().unwrap();
        let store = test_store(&tmp);

        let mut event = make_event("user_a", "browse", "https://binance.com/trade");
        event.details = "Checking crypto trade positions".into();
        store.record(event).unwrap();

        let events = store.query(&TelemetryQuery::default()).unwrap();
        assert_eq!(events[0].alert_level, AlertLevel::Critical);
        assert!(events[0].alert_reason.contains("CRITICAL"));
        assert!(events[0].alert_reason.contains("binance.com"));
    }

    #[test]
    fn alert_classification_detects_gambling() {
        let tmp = TempDir::new().unwrap();
        let store = test_store(&tmp);

        store
            .record(make_event("user_a", "browse", "https://my-casino-site.com"))
            .unwrap();

        let events = store.query(&TelemetryQuery::default()).unwrap();
        assert_eq!(events[0].alert_level, AlertLevel::Warning);
        assert!(events[0].alert_reason.contains("casino"));
    }

    #[test]
    fn no_alert_for_normal_activity() {
        let tmp = TempDir::new().unwrap();
        let store = test_store(&tmp);

        store
            .record(make_event("user_a", "browse", "https://news.example.com"))
            .unwrap();

        let events = store.query(&TelemetryQuery::default()).unwrap();
        assert_eq!(events[0].alert_level, AlertLevel::None);
        assert!(events[0].alert_reason.is_empty());
    }

    #[test]
    fn summary_returns_statistics() {
        let tmp = TempDir::new().unwrap();
        let store = test_store(&tmp);

        store
            .record(make_event("user_a", "browse", "https://example.com"))
            .unwrap();
        store.record(make_event("user_b", "shell", "")).unwrap();
        store
            .record(make_event(
                "user_a",
                "browse",
                "https://binance.com/deposit",
            ))
            .unwrap();

        let summary = store.summary().unwrap();
        assert_eq!(summary.total_events, 3);
        assert_eq!(summary.total_users, 2);
        assert!(summary.total_alerts >= 1);
        assert!(!summary.top_countries.is_empty());
    }

    #[test]
    fn query_with_search_filter() {
        let tmp = TempDir::new().unwrap();
        let store = test_store(&tmp);

        let mut evt = make_event("user_a", "browse", "https://example.com");
        evt.details = "Checking weather forecast".into();
        store.record(evt).unwrap();

        store
            .record(make_event("user_a", "browse", "https://other.com"))
            .unwrap();

        let results = store
            .query(&TelemetryQuery {
                search: Some("weather".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].details.contains("weather"));
    }

    #[test]
    fn prune_respects_max_events() {
        let tmp = TempDir::new().unwrap();
        let config = TelemetryConfig {
            enabled: true,
            max_events: 3,
            ..test_config()
        };
        let store = TelemetryStore::new(tmp.path(), config).unwrap();

        for i in 0..5 {
            store
                .record(make_event(
                    &format!("user_{i}"),
                    "browse",
                    "https://example.com",
                ))
                .unwrap();
        }

        let all = store.query(&TelemetryQuery::default()).unwrap();
        assert!(all.len() <= 3);
    }
}
