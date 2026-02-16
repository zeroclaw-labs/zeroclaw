use crate::config::Config;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

const DAEMON_STALE_SECONDS: i64 = 30;
const SCHEDULER_STALE_SECONDS: i64 = 120;
const CHANNEL_STALE_SECONDS: i64 = 300;

pub fn run(config: &Config) -> Result<()> {
    let state_file = crate::daemon::state_file_path(config);
    if !state_file.exists() {
        println!("🩺 ZeroClaw Doctor");
        println!("  ❌ daemon state file not found: {}", state_file.display());
        println!("  💡 Start daemon with: zeroclaw daemon");
        return Ok(());
    }

    let raw = std::fs::read_to_string(&state_file)
        .with_context(|| format!("Failed to read {}", state_file.display()))?;
    let snapshot: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", state_file.display()))?;

    println!("🩺 ZeroClaw Doctor");
    println!("  State file: {}", state_file.display());

    let updated_at = snapshot
        .get("updated_at")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    if let Ok(ts) = DateTime::parse_from_rfc3339(updated_at) {
        let age = Utc::now()
            .signed_duration_since(ts.with_timezone(&Utc))
            .num_seconds();
        if age <= DAEMON_STALE_SECONDS {
            println!("  ✅ daemon heartbeat fresh ({age}s ago)");
        } else {
            println!("  ❌ daemon heartbeat stale ({age}s ago)");
        }
    } else {
        println!("  ❌ invalid daemon timestamp: {updated_at}");
    }

    let mut channel_count = 0_u32;
    let mut stale_channels = 0_u32;

    if let Some(components) = snapshot
        .get("components")
        .and_then(serde_json::Value::as_object)
    {
        if let Some(scheduler) = components.get("scheduler") {
            let scheduler_ok = scheduler
                .get("status")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|s| s == "ok");

            let scheduler_last_ok = scheduler
                .get("last_ok")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_rfc3339)
                .map_or(i64::MAX, |dt| {
                    Utc::now().signed_duration_since(dt).num_seconds()
                });

            if scheduler_ok && scheduler_last_ok <= SCHEDULER_STALE_SECONDS {
                println!("  ✅ scheduler healthy (last ok {scheduler_last_ok}s ago)");
            } else {
                println!(
                    "  ❌ scheduler unhealthy/stale (status_ok={scheduler_ok}, age={scheduler_last_ok}s)"
                );
            }
        } else {
            println!("  ❌ scheduler component missing");
        }

        for (name, component) in components {
            if !name.starts_with("channel:") {
                continue;
            }

            channel_count += 1;
            let status_ok = component
                .get("status")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|s| s == "ok");
            let age = component
                .get("last_ok")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_rfc3339)
                .map_or(i64::MAX, |dt| {
                    Utc::now().signed_duration_since(dt).num_seconds()
                });

            if status_ok && age <= CHANNEL_STALE_SECONDS {
                println!("  ✅ {name} fresh (last ok {age}s ago)");
            } else {
                stale_channels += 1;
                println!("  ❌ {name} stale/unhealthy (status_ok={status_ok}, age={age}s)");
            }
        }
    }

    if channel_count == 0 {
        println!("  ℹ️ no channel components tracked in state yet");
    } else {
        println!("  Channel summary: {channel_count} total, {stale_channels} stale");
    }

    Ok(())
}

fn parse_rfc3339(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use serde_json::json;
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Config {
        let mut config = Config::default();
        config.workspace_dir = tmp.path().join("workspace");
        config.config_path = tmp.path().join("config.toml");
        config
    }

    #[test]
    fn parse_rfc3339_accepts_valid_timestamp() {
        let parsed = parse_rfc3339("2025-01-02T03:04:05Z");
        assert!(parsed.is_some());
    }

    #[test]
    fn parse_rfc3339_rejects_invalid_timestamp() {
        let parsed = parse_rfc3339("not-a-timestamp");
        assert!(parsed.is_none());
    }

    #[test]
    fn run_returns_ok_when_state_file_missing() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let result = run(&config);

        assert!(result.is_ok());
    }

    #[test]
    fn run_returns_error_for_invalid_json_state_file() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let state_file = crate::daemon::state_file_path(&config);

        std::fs::write(&state_file, "not-json").unwrap();

        let result = run(&config);

        assert!(result.is_err());
        let error_text = result.unwrap_err().to_string();
        assert!(error_text.contains("Failed to parse"));
    }

    #[test]
    fn run_accepts_well_formed_state_snapshot() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let state_file = crate::daemon::state_file_path(&config);

        let now = Utc::now().to_rfc3339();
        let snapshot = json!({
            "updated_at": now,
            "components": {
                "scheduler": {
                    "status": "ok",
                    "last_ok": now,
                    "last_error": null,
                    "updated_at": now,
                    "restart_count": 0
                },
                "channel:discord": {
                    "status": "ok",
                    "last_ok": now,
                    "last_error": null,
                    "updated_at": now,
                    "restart_count": 0
                }
            }
        });

        std::fs::write(&state_file, serde_json::to_vec_pretty(&snapshot).unwrap()).unwrap();

        let result = run(&config);

        assert!(result.is_ok());
    }
}
