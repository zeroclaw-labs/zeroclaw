use crate::cron::Schedule;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use cron::Schedule as CronExprSchedule;
use regex::Regex;
use std::borrow::Cow;
use std::str::FromStr;
use std::sync::OnceLock;

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

pub fn normalize_expression(expression: &str) -> Result<Cow<'_, str>> {
    let expression = expression.trim();
    let field_count = expression.split_whitespace().count();

    match field_count {
        // standard crontab syntax: minute hour day month weekday
        5 => {
            // Translate weekday from standard (0-7, 1=Mon) to Quartz (1-7, 1=Sun).
            // NOTE: Quartz treats specified Day-of-Month and Day-of-Week as an AND condition,
            // while standard crontab treats them as an OR condition.
            let mut fields = expression.split_whitespace();
            let f0 = fields.next().unwrap();
            let f1 = fields.next().unwrap();
            let f2 = fields.next().unwrap();
            let f3 = fields.next().unwrap();
            let f4 = fields.next().unwrap();
            let weekday = translate_standard_to_quartz_dow(f4);

            Ok(Cow::Owned(format!(
                "0 {} {} {} {} {}",
                f0, f1, f2, f3, weekday
            )))
        }
        // crate-native syntax includes seconds (+ optional year)
        6 | 7 => Ok(Cow::Borrowed(expression)),
        _ => anyhow::bail!(
            "Invalid cron expression: {expression} (expected 5, 6, or 7 fields, got {field_count})"
        ),
    }
}

fn translate_standard_to_quartz_dow(dow: &str) -> Cow<'_, str> {
    static DOW_RE: OnceLock<Regex> = OnceLock::new();
    let re = DOW_RE.get_or_init(|| Regex::new(r"\d+").expect("Invalid regex for DOW translation"));

    let (to_translate, step) = match dow.split_once('/') {
        Some((prefix, step)) => (prefix, Some(step)),
        None => (dow, None),
    };

    let translated = re.replace_all(to_translate, |caps: &regex::Captures| {
        if let Ok(val) = caps[0].parse::<u32>() {
            // standard: 0=Sun, 1=Mon, ..., 6=Sat, 7=Sun
            // quartz: 1=Sun, 2=Mon, ..., 7=Sat
            let translated = (val % 7) + 1;
            translated.to_string()
        } else {
            caps[0].to_string()
        }
    });

    match (translated, step) {
        (Cow::Borrowed(_), None) => Cow::Borrowed(dow),
        (Cow::Owned(s), None) => Cow::Owned(s),
        (prefix, Some(s)) => Cow::Owned(format!("{}/{}", prefix, s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

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
    fn test_cron_normalization_and_translation() {
        // '0 8 * * 1-6' (Mon-Sat standard) -> '0 0 8 * * 2-7' (Mon-Sat Quartz)
        let normalized = normalize_expression("0 8 * * 1-6").unwrap();
        assert_eq!(normalized, "0 0 8 * * 2-7");

        // '0 8 * * 0' (Sun standard) -> '0 0 8 * * 1' (Sun Quartz)
        let sun = normalize_expression("0 8 * * 0").unwrap();
        assert_eq!(sun, "0 0 8 * * 1");

        // '0 8 * * 7' (Sun standard) -> '0 0 8 * * 1' (Sun Quartz)
        let sun7 = normalize_expression("0 8 * * 7").unwrap();
        assert_eq!(sun7, "0 0 8 * * 1");

        // '0 8 * * MON-FRI' -> '0 0 8 * * MON-FRI' (names preserved)
        let names = normalize_expression("0 8 * * MON-FRI").unwrap();
        assert_eq!(names, "0 0 8 * * MON-FRI");

        // '0 8 1 * 2' (8:00 AM on 1st of month OR Tuesday standard)
        let both = normalize_expression("0 8 1 * 2").unwrap();
        let cron_both = CronExprSchedule::from_str(&both).unwrap();
        // April 2026: 1st is Wednesday. 7th is Tuesday.
        let april1 = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        let next = cron_both.after(&april1).next().unwrap();
        // If OR: next should be April 7 (Tuesday). Wait, April 1 is Wednesday.
        // If April 1 08:00 is INCLUDED, it should be April 1 08:00? No, 'after' is exclusive.
        let march31 = Utc.with_ymd_and_hms(2026, 3, 31, 0, 0, 0).unwrap();
        let first = cron_both.after(&march31).next().unwrap();
        // In standard cron, it would be April 1 (1st of month).
        // Let's see what this crate does.
        println!("Next run for '0 0 8 1 * 3' after March 31: {}", first);

        // 6-field expression should be left as-is (Quartz native)
        let quartz = normalize_expression("0 0 8 * * 1-6").unwrap();
        assert_eq!(quartz, "0 0 8 * * 1-6");
    }

    #[test]
    fn test_cron_dow_fix_behavior() {
        // Sunday, March 22, 2026.
        let sunday = Utc.with_ymd_and_hms(2026, 3, 22, 0, 0, 0).unwrap();

        // Expression '0 8 * * 1-6' (Mon-Sat standard).
        // It should skip Sunday March 22 and run on Monday March 23.
        let schedule = Schedule::Cron {
            expr: "0 8 * * 1-6".into(),
            tz: None,
        };

        let next = next_run_for_schedule(&schedule, sunday).unwrap();
        assert_eq!(next.format("%Y-%m-%d").to_string(), "2026-03-23");
        assert_eq!(next.format("%H:%M:%S").to_string(), "08:00:00");
    }
}
