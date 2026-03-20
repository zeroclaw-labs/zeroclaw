use crate::cron::Schedule;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use cron::Schedule as CronExprSchedule;
use std::str::FromStr;

pub fn next_run_for_schedule(schedule: &Schedule, from: DateTime<Utc>) -> Result<DateTime<Utc>> {
    match schedule {
        Schedule::Cron { expr, tz } => {
            let normalized = normalize_expression(expr)?;
            let cron = CronExprSchedule::from_str(&normalized)
                .with_context(|| format!("Invalid cron expression: {expr}"))?;

            if let Some(tz_name) = tz {
                let timezone = chrono_tz::Tz::from_str(tz_name)
                    .with_context(|| format!("Invalid IANA timezone: {tz_name}"))?;
                let localized_from = from.with_timezone(&timezone);
                let next_local = cron.after(&localized_from).next().ok_or_else(|| {
                    anyhow::anyhow!("No future occurrence for expression: {expr}")
                })?;
                Ok(next_local.with_timezone(&Utc))
            } else {
                cron.after(&from)
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("No future occurrence for expression: {expr}"))
            }
        }
        Schedule::At { at } => Ok(*at),
        Schedule::Every { every_ms } => {
            if *every_ms == 0 {
                anyhow::bail!("Invalid schedule: every_ms must be > 0");
            }
            let ms = i64::try_from(*every_ms).context("every_ms is too large")?;
            let delta = ChronoDuration::milliseconds(ms);
            from.checked_add_signed(delta)
                .ok_or_else(|| anyhow::anyhow!("every_ms overflowed DateTime"))
        }
    }
}

pub fn validate_schedule(schedule: &Schedule, now: DateTime<Utc>) -> Result<()> {
    match schedule {
        Schedule::Cron { expr, .. } => {
            let _ = normalize_expression(expr)?;
            let _ = next_run_for_schedule(schedule, now)?;
            Ok(())
        }
        Schedule::At { at } => {
            if *at <= now {
                anyhow::bail!("Invalid schedule: 'at' must be in the future");
            }
            Ok(())
        }
        Schedule::Every { every_ms } => {
            if *every_ms == 0 {
                anyhow::bail!("Invalid schedule: every_ms must be > 0");
            }
            Ok(())
        }
    }
}

pub fn schedule_cron_expression(schedule: &Schedule) -> Option<String> {
    match schedule {
        Schedule::Cron { expr, .. } => Some(expr.clone()),
        _ => None,
    }
}

pub fn normalize_expression(expression: &str) -> Result<String> {
    let expression = expression.trim();
    let mut fields: Vec<&str> = expression.split_whitespace().collect();
    let field_count = fields.len();

    match field_count {
        // standard crontab syntax: minute hour day month weekday
        // prepend seconds field and remap weekday (last field)
        5 => {
            let weekday = remap_weekday_field(fields[4])?;
            Ok(format!(
                "0 {} {} {} {} {}",
                fields[0], fields[1], fields[2], fields[3], weekday
            ))
        }
        // 6-field (sec min hour dom month dow) or
        // 7-field (sec min hour dom month dow year)
        6 | 7 => {
            let weekday = remap_weekday_field(fields[5])?;
            fields[5] = &weekday;
            Ok(fields.join(" "))
        }
        _ => anyhow::bail!(
            "Invalid cron expression: {expression} (expected 5, 6, or 7 fields, got {field_count})"
        ),
    }
}

/// Remap a standard-cron weekday field to the `cron` crate's 1-based mapping.
///
/// Standard cron: 0=Sun, 1=Mon, 2=Tue, ..., 6=Sat, 7=Sun
/// `cron` crate:  1=Sun, 2=Mon, 3=Tue, ..., 7=Sat
///
/// Named days (MON, FRI, etc.) are passed through unchanged because the crate
/// already maps them correctly via `ordinal_from_name`.
fn remap_weekday_field(field: &str) -> Result<String> {
    // Wildcards and step-only patterns need no remapping
    if field == "*" || field == "?" {
        return Ok(field.to_string());
    }

    // Handle */step — no remapping needed, the crate expands * to its full range
    if field.starts_with("*/") {
        return Ok(field.to_string());
    }

    // Split on commas to handle lists like "1,3,5"
    let parts: Vec<&str> = field.split(',').collect();
    let mut remapped_parts = Vec::with_capacity(parts.len());

    for part in parts {
        remapped_parts.push(remap_weekday_atom(part)?);
    }

    Ok(remapped_parts.join(","))
}

/// Remap a single weekday atom. An atom is one of:
///   - a bare number: "1"
///   - a range: "1-5"
///   - a range with step: "1-5/2"
///   - a named day: "MON"
///   - a named range: "MON-FRI"
fn remap_weekday_atom(atom: &str) -> Result<String> {
    // Split off an optional /step suffix
    let (base, step_suffix) = match atom.split_once('/') {
        Some((b, s)) => (b, format!("/{s}")),
        None => (atom, String::new()),
    };

    // Check for range
    if let Some((start, end)) = base.split_once('-') {
        let start_mapped = remap_single_weekday_value(start)?;
        let end_mapped = remap_single_weekday_value(end)?;
        Ok(format!("{start_mapped}-{end_mapped}{step_suffix}"))
    } else {
        let mapped = remap_single_weekday_value(base)?;
        Ok(format!("{mapped}{step_suffix}"))
    }
}

/// Remap a single weekday value (number or name).
/// Numbers 0-7 are shifted to the crate's 1-7 range.
/// Named days are passed through as-is.
fn remap_single_weekday_value(val: &str) -> Result<String> {
    match val.parse::<u8>() {
        Ok(n) => {
            let mapped = match n {
                0 | 7 => 1, // Sunday
                1..=6 => n + 1,
                _ => anyhow::bail!("Invalid weekday number: {n} (expected 0-7)"),
            };
            Ok(mapped.to_string())
        }
        Err(_) => {
            // Named day — pass through, the crate handles it
            Ok(val.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone};

    #[test]
    fn next_run_for_schedule_supports_every_and_at() {
        let now = Utc::now();
        let every = Schedule::Every { every_ms: 60_000 };
        let next = next_run_for_schedule(&every, now).unwrap();
        assert!(next > now);

        let at = now + ChronoDuration::minutes(10);
        let at_schedule = Schedule::At { at };
        let next_at = next_run_for_schedule(&at_schedule, now).unwrap();
        assert_eq!(next_at, at);
    }

    #[test]
    fn next_run_for_schedule_supports_timezone() {
        let from = Utc.with_ymd_and_hms(2026, 2, 16, 0, 0, 0).unwrap();
        let schedule = Schedule::Cron {
            expr: "0 9 * * *".into(),
            tz: Some("America/Los_Angeles".into()),
        };

        let next = next_run_for_schedule(&schedule, from).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 2, 16, 17, 0, 0).unwrap());
    }

    // ── Weekday remapping tests ──────────────────────────────────────

    #[test]
    fn normalize_expression_remaps_weekday_range_1_5_to_mon_fri() {
        let norm = normalize_expression("0 9 * * 1-5").unwrap();
        // 1-5 (Mon-Fri standard) → 2-6 (Mon-Fri in crate)
        assert_eq!(norm, "0 0 9 * * 2-6");
    }

    #[test]
    fn weekday_0_and_7_both_map_to_sunday() {
        let norm_0 = normalize_expression("0 9 * * 0").unwrap();
        let norm_7 = normalize_expression("0 9 * * 7").unwrap();
        // Both should map to crate's 1 (Sunday)
        assert_eq!(norm_0, "0 0 9 * * 1");
        assert_eq!(norm_7, "0 0 9 * * 1");
    }

    #[test]
    fn weekday_list_remapped() {
        let norm = normalize_expression("0 9 * * 1,3,5").unwrap();
        assert_eq!(norm, "0 0 9 * * 2,4,6");
    }

    #[test]
    fn weekday_named_days_pass_through() {
        let norm = normalize_expression("0 9 * * MON-FRI").unwrap();
        assert_eq!(norm, "0 0 9 * * MON-FRI");
    }

    #[test]
    fn weekday_star_and_step_unchanged() {
        assert_eq!(normalize_expression("0 9 * * *").unwrap(), "0 0 9 * * *");
        assert_eq!(
            normalize_expression("0 9 * * */2").unwrap(),
            "0 0 9 * * */2"
        );
    }

    #[test]
    fn weekday_1_5_schedules_mon_fri_not_sun_thu() {
        // 2026-03-16 is a Monday
        let monday = Utc.with_ymd_and_hms(2026, 3, 16, 0, 0, 0).unwrap();
        let schedule = Schedule::Cron {
            expr: "0 9 * * 1-5".into(),
            tz: None,
        };

        // Collect the next 5 runs starting from Monday 00:00
        let mut runs = Vec::new();
        let mut from = monday;
        for _ in 0..5 {
            let next = next_run_for_schedule(&schedule, from).unwrap();
            runs.push(next);
            from = next;
        }

        // Should be Mon-Fri at 09:00
        let expected_days: Vec<chrono::Weekday> = vec![
            chrono::Weekday::Mon,
            chrono::Weekday::Tue,
            chrono::Weekday::Wed,
            chrono::Weekday::Thu,
            chrono::Weekday::Fri,
        ];
        for (run, expected_day) in runs.iter().zip(expected_days.iter()) {
            assert_eq!(
                run.weekday(),
                *expected_day,
                "Expected {:?}, got {:?} for {}",
                expected_day,
                run.weekday(),
                run
            );
        }
    }

    #[test]
    fn weekday_0_schedules_sunday() {
        // 2026-03-16 is a Monday, next Sunday is 2026-03-22
        let monday = Utc.with_ymd_and_hms(2026, 3, 16, 0, 0, 0).unwrap();
        let schedule = Schedule::Cron {
            expr: "0 9 * * 0".into(),
            tz: None,
        };
        let next = next_run_for_schedule(&schedule, monday).unwrap();
        assert_eq!(next.weekday(), chrono::Weekday::Sun);
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 3, 22, 9, 0, 0).unwrap());
    }

    #[test]
    fn weekday_7_schedules_sunday() {
        let monday = Utc.with_ymd_and_hms(2026, 3, 16, 0, 0, 0).unwrap();
        let schedule = Schedule::Cron {
            expr: "0 9 * * 7".into(),
            tz: None,
        };
        let next = next_run_for_schedule(&schedule, monday).unwrap();
        assert_eq!(next.weekday(), chrono::Weekday::Sun);
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 3, 22, 9, 0, 0).unwrap());
    }
}
