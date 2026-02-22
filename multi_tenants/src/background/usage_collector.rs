use crate::db::pool::DbPool;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::time::interval;
use tracing::{error, info, warn};
use uuid::Uuid;

const COLLECT_INTERVAL_MINS: u64 = 5;
const METRICS_TIMEOUT_SECS: u64 = 5;

// ---------------------------------------------------------------------------
// Background loop
// ---------------------------------------------------------------------------

pub async fn run(db: DbPool, mut shutdown_rx: broadcast::Receiver<()>) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(METRICS_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            error!("usage_collector: failed to build HTTP client: {}", e);
            return;
        }
    };

    let mut ticker = interval(Duration::from_secs(COLLECT_INTERVAL_MINS * 60));

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                collect_all(&db, &client).await;
            }
            _ = shutdown_rx.recv() => {
                info!("usage_collector: shutdown signal received");
                break;
            }
        }
    }
}

/// Scrape metrics from all running tenants and upsert into usage_metrics.
async fn collect_all(db: &DbPool, client: &reqwest::Client) {
    let tenants: Vec<(String, u16)> = match db.read(|conn| {
        let mut stmt = conn.prepare("SELECT id, port FROM tenants WHERE status = 'running'")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u16))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }) {
        Ok(t) => t,
        Err(e) => {
            error!("usage_collector: failed to load running tenants: {}", e);
            return;
        }
    };

    for (tenant_id, port) in tenants {
        collect_tenant(db, client, &tenant_id, port).await;
    }
}

async fn collect_tenant(db: &DbPool, client: &reqwest::Client, tenant_id: &str, port: u16) {
    let url = format!("http://127.0.0.1:{}/metrics", port);
    let text = match client.get(&url).send().await {
        Ok(resp) => match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                warn!(
                    "usage_collector: failed to read metrics body for tenant {}: {}",
                    tenant_id, e
                );
                return;
            }
        },
        Err(e) => {
            warn!(
                "usage_collector: metrics request failed for tenant {}: {}",
                tenant_id, e
            );
            return;
        }
    };

    let messages = parse_metric(&text, "zeroclaw_messages_total");
    let tokens_in = parse_labeled_metric(&text, "zeroclaw_tokens_total", "direction", "input");
    let tokens_out = parse_labeled_metric(&text, "zeroclaw_tokens_total", "direction", "output");

    // Skip UPSERT if no metrics were parsed — avoids overwriting real data with zeros
    // when a tenant's metrics endpoint is temporarily unreachable or returns unexpected format.
    if messages.is_none() && tokens_in.is_none() && tokens_out.is_none() {
        tracing::debug!(
            "usage_collector: no metrics parsed for tenant {}, skipping",
            tenant_id
        );
        return;
    }

    let m = messages.unwrap_or(0);
    let ti = tokens_in.unwrap_or(0);
    let to = tokens_out.unwrap_or(0);

    // Period key: current UTC hour truncated to YYYY-MM-DDTHH:00:00Z.
    let period = chrono::Utc::now().format("%Y-%m-%dT%H:00:00Z").to_string();
    let id = Uuid::new_v4().to_string();

    if let Err(e) = db.write(|conn| {
        conn.execute(
            "INSERT INTO usage_metrics (id, tenant_id, period, messages, tokens_in, tokens_out)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(tenant_id, period) DO UPDATE SET
               messages   = ?4,
               tokens_in  = ?5,
               tokens_out = ?6",
            rusqlite::params![id, tenant_id, period, m, ti, to],
        )?;
        Ok(())
    }) {
        error!(
            "usage_collector: failed to upsert usage for tenant {}: {}",
            tenant_id, e
        );
    }
}

// ---------------------------------------------------------------------------
// Metric parsing helpers (pub for unit tests)
// ---------------------------------------------------------------------------

/// Parse a simple counter value from Prometheus text format.
/// Returns `None` if metric not found or value unparseable.
///
/// Looks for lines matching: `<name> <value>` or `<name>{...} <value>`.
/// Lines starting with `#` are comments/metadata and are skipped.
pub fn parse_metric(text: &str, name: &str) -> Option<i64> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }

        // Accept "name value" and "name{...} value"
        if let Some(rest) = line.strip_prefix(name) {
            if rest.starts_with(' ') || rest.starts_with('{') {
                // Extract the value token (last whitespace-separated segment)
                if let Some(value_str) = rest.split_whitespace().last() {
                    // Parse as float first to support "1.0e+03" notation, then truncate.
                    if let Ok(f) = value_str.parse::<f64>() {
                        return Some(f as i64);
                    }
                }
            }
        }
    }
    None
}

/// Parse a labeled metric value from Prometheus text format.
/// e.g. `parse_labeled_metric(text, "zeroclaw_tokens_total", "direction", "input")`
///
/// Returns `None` if no matching line found or value unparseable.
pub fn parse_labeled_metric(
    text: &str,
    name: &str,
    label_key: &str,
    label_value: &str,
) -> Option<i64> {
    // We look for lines like: name{...,key="value",...} numeric
    let target_label = format!("{}=\"{}\"", label_key, label_value);

    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }

        if !line.starts_with(name) {
            continue;
        }

        // Find label block
        let brace_start = match line.find('{') {
            Some(i) => i,
            None => continue,
        };
        let brace_end = match line.find('}') {
            Some(i) => i,
            None => continue,
        };

        if brace_end <= brace_start {
            continue;
        }

        let labels = &line[brace_start + 1..brace_end];
        if !labels.contains(&target_label as &str) {
            continue;
        }

        // Value follows the closing brace.
        let after_brace = line[brace_end + 1..].trim();
        if let Some(value_str) = after_brace.split_whitespace().next() {
            if let Ok(f) = value_str.parse::<f64>() {
                return Some(f as i64);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_METRICS: &str = r#"
# HELP zeroclaw_messages_total Total number of messages processed
# TYPE zeroclaw_messages_total counter
zeroclaw_messages_total 42
# HELP zeroclaw_tokens_total Total tokens by direction
# TYPE zeroclaw_tokens_total counter
zeroclaw_tokens_total{direction="input"} 1234
zeroclaw_tokens_total{direction="output"} 567
"#;

    #[test]
    fn test_parse_metric_simple_counter() {
        let val = parse_metric(SAMPLE_METRICS, "zeroclaw_messages_total");
        assert_eq!(val, Some(42));
    }

    #[test]
    fn test_parse_labeled_metric() {
        let tokens_in = parse_labeled_metric(
            SAMPLE_METRICS,
            "zeroclaw_tokens_total",
            "direction",
            "input",
        );
        assert_eq!(tokens_in, Some(1234));

        let tokens_out = parse_labeled_metric(
            SAMPLE_METRICS,
            "zeroclaw_tokens_total",
            "direction",
            "output",
        );
        assert_eq!(tokens_out, Some(567));
    }

    #[test]
    fn test_parse_empty_metrics() {
        assert_eq!(parse_metric("", "zeroclaw_messages_total"), None);
        assert_eq!(
            parse_labeled_metric("", "zeroclaw_tokens_total", "direction", "input"),
            None
        );
    }

    #[test]
    fn test_parse_metric_with_comments() {
        let text = "# HELP zeroclaw_messages_total docs\n# TYPE zeroclaw_messages_total counter\n";
        assert_eq!(parse_metric(text, "zeroclaw_messages_total"), None);
    }

    #[test]
    fn test_parse_metric_with_labels_no_match() {
        // Looking for "output" but only "input" present.
        let text = "zeroclaw_tokens_total{direction=\"input\"} 999\n";
        assert_eq!(
            parse_labeled_metric(text, "zeroclaw_tokens_total", "direction", "output"),
            None
        );
    }

    #[test]
    fn test_parse_metric_float_value() {
        let text = "zeroclaw_messages_total 1000.0\n";
        assert_eq!(parse_metric(text, "zeroclaw_messages_total"), Some(1000));
    }

    #[test]
    fn test_parse_labeled_metric_multiple_labels() {
        let text = "zeroclaw_tokens_total{model=\"gpt-4\",direction=\"input\"} 555\n";
        assert_eq!(
            parse_labeled_metric(text, "zeroclaw_tokens_total", "direction", "input"),
            Some(555)
        );
    }
}
