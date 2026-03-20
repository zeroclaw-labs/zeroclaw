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
    let field_count = expression.split_whitespace().count();

    match field_count {
        // standard crontab syntax: minute hour day month weekday
        // The `cron` crate uses 1-indexed weekdays (1=Sunday..7=Saturday),
        // but standard crontab uses 0-indexed (0=Sunday..6=Saturday, 7=Sunday).
        // We must remap the weekday field before prepending the seconds field.
        5 => {
            let fields: Vec<&str> = expression.split_whitespace().collect();
            let converted_weekday = convert_weekday_field(fields[4]);
            Ok(format!(
                "0 {} {} {} {} {}",
                fields[0], fields[1], fields[2], fields[3], converted_weekday
            ))
        }
        // crate-native syntax includes seconds (+ optional year)
        // Users of 6/7-field syntax are already using crate semantics — no conversion.
        6 | 7 => Ok(expression.to_string()),
        _ => anyhow::bail!(
            "Invalid cron expression: {expression} (expected 5, 6, or 7 fields, got {field_count})"
        ),
    }
}

/// Convert a standard-crontab weekday field to `cron`-crate semantics.
///
/// Standard crontab: 0=Sunday, 1=Monday … 6=Saturday, 7=Sunday (alias)
/// `cron` crate:     1=Sunday, 2=Monday … 7=Saturday
///
/// Handles: single numbers (`5`), ranges (`1-5`), lists (`1,3,5`),
/// step values (`1-5/2`), wildcards (`*`, `?`), and named days (`MON-FRI`).
/// Named days are left untouched because the crate handles them correctly.
fn convert_weekday_field(field: &str) -> String {
    // Split on comma to handle lists like "1,3,5" or "1-3,5"
    field
        .split(',')
        .map(convert_weekday_part)
        .collect::<Vec<_>>()
        .join(",")
}

/// Convert a single comma-separated part of a weekday field.
/// E.g. "1-5", "1-5/2", "5", "*", "*/2", "MON-FRI"
fn convert_weekday_part(part: &str) -> String {
    // Wildcards pass through
    if part == "*" || part == "?" {
        return part.to_string();
    }

    // Handle step suffix: "1-5/2" → convert "1-5", keep "/2"
    let (base, step) = match part.split_once('/') {
        Some((b, s)) => (b, Some(s)),
        None => (part, None),
    };

    let converted_base = if base == "*" || base == "?" {
        base.to_string()
    } else if let Some((start, end)) = base.split_once('-') {
        // Range: "1-5"
        let new_start = remap_weekday_number(start);
        let new_end = remap_weekday_number(end);
        format!("{new_start}-{new_end}")
    } else {
        // Single value: "5"
        remap_weekday_number(base)
    };

    match step {
        Some(s) => format!("{converted_base}/{s}"),
        None => converted_base,
    }
}

/// Remap a single weekday token from standard-crontab to crate semantics.
/// If the token is a named day (e.g. "MON") or not a valid number, return as-is.
fn remap_weekday_number(token: &str) -> String {
    match token.parse::<u8>() {
        Ok(n) => {
            let remapped = match n {
                0 | 7 => 1, // Sunday
                1..=6 => n + 1,
                _ => return token.to_string(), // out of range — let crate validate
            };
            remapped.to_string()
        }
        Err(_) => token.to_string(), // named day like MON, TUE — pass through
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn weekday_single_numbers() {
        assert_eq!(convert_weekday_field("0"), "1"); // Sun → Sun
        assert_eq!(convert_weekday_field("1"), "2"); // Mon → Mon
        assert_eq!(convert_weekday_field("5"), "6"); // Fri → Fri
        assert_eq!(convert_weekday_field("6"), "7"); // Sat → Sat
        assert_eq!(convert_weekday_field("7"), "1"); // Sun alias → Sun
    }

    #[test]
    fn weekday_ranges() {
        assert_eq!(convert_weekday_field("1-5"), "2-6"); // Mon-Fri
        assert_eq!(convert_weekday_field("0-6"), "1-7"); // Sun-Sat
        assert_eq!(convert_weekday_field("0-4"), "1-5"); // Sun-Thu
    }

    #[test]
    fn weekday_lists() {
        assert_eq!(convert_weekday_field("1,3,5"), "2,4,6"); // Mon,Wed,Fri
        assert_eq!(convert_weekday_field("0,6"), "1,7"); // Sun,Sat
    }

    #[test]
    fn weekday_steps() {
        assert_eq!(convert_weekday_field("1-5/2"), "2-6/2"); // Mon-Fri step 2
        assert_eq!(convert_weekday_field("*/2"), "*/2"); // wildcard step
    }

    #[test]
    fn weekday_wildcards() {
        assert_eq!(convert_weekday_field("*"), "*");
        assert_eq!(convert_weekday_field("?"), "?");
    }

    #[test]
    fn weekday_named_days() {
        assert_eq!(convert_weekday_field("MON-FRI"), "MON-FRI");
        assert_eq!(convert_weekday_field("MON"), "MON");
        assert_eq!(convert_weekday_field("SUN,SAT"), "SUN,SAT");
    }

    #[test]
    fn weekday_mixed_list() {
        assert_eq!(convert_weekday_field("1,3-5,7"), "2,4-6,1");
    }

    #[test]
    fn normalize_5field_converts_weekday() {
        // "0 9 * * 1-5" (Mon-Fri at 9am) → "0 0 9 * * 2-6"
        assert_eq!(
            normalize_expression("0 9 * * 1-5").unwrap(),
            "0 0 9 * * 2-6"
        );
    }

    #[test]
    fn normalize_6field_unchanged() {
        // 6-field uses crate semantics directly — no conversion
        assert_eq!(
            normalize_expression("0 0 9 * * 1-5").unwrap(),
            "0 0 9 * * 1-5"
        );
    }

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
}
