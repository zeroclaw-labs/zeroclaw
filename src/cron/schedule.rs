use crate::cron::{ConditionalSchedule, Schedule};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use cron::Schedule as CronExprSchedule;
use std::str::FromStr;

/// Compute the next run time considering an optional conditional schedule.
///
/// If `conditional` is active at `from`, uses the conditional schedule;
/// otherwise uses the primary `schedule`. If the conditional schedule would
/// produce a next_run after the condition window ends, falls back to the
/// primary schedule instead.
pub fn effective_next_run(
    schedule: &Schedule,
    conditional: Option<&ConditionalSchedule>,
    from: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    if let Some(cond) = conditional {
        if cond.is_active(from) {
            let candidate = next_run_for_schedule(&cond.schedule, from)?;
            // Only use the conditional schedule if the next occurrence falls
            // within (or at) the condition window end.
            if candidate <= cond.condition_end {
                return Ok(candidate);
            }
            // Condition will expire before the next conditional run;
            // fall through to the primary schedule.
        }
    }
    next_run_for_schedule(schedule, from)
}

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
        // Normalize weekday field from standard crontab semantics (0/7=Sun, 1=Mon, …, 6=Sat)
        // to cron-crate semantics (1=Sun, 2=Mon, …, 7=Sat).
        5 => {
            let mut fields: Vec<&str> = expression.split_whitespace().collect();
            let weekday = fields[4];
            let normalized_weekday = normalize_weekday_field(weekday)?;
            fields[4] = &normalized_weekday;
            Ok(format!(
                "0 {} {} {} {} {}",
                fields[0], fields[1], fields[2], fields[3], fields[4]
            ))
        }
        // crate-native syntax includes seconds (+ optional year)
        6 | 7 => Ok(expression.to_string()),
        _ => anyhow::bail!(
            "Invalid cron expression: {expression} (expected 5, 6, or 7 fields, got {field_count})"
        ),
    }
}

/// Translate a single numeric weekday value from standard crontab semantics
/// (0 or 7 = Sunday, 1 = Monday, …, 6 = Saturday) to cron-crate semantics
/// (1 = Sunday, 2 = Monday, …, 7 = Saturday).
fn translate_weekday_value(val: u8) -> Result<u8> {
    match val {
        0 | 7 => Ok(1), // Sunday
        1..=6 => Ok(val + 1),
        _ => anyhow::bail!("Invalid weekday value: {val} (expected 0-7)"),
    }
}

/// Normalize the weekday field of a 5-field cron expression from standard
/// crontab numbering to cron-crate numbering. Passes through `*`, named days
/// (e.g. `MON`, `MON-FRI`), and already-valid tokens unchanged.
fn normalize_weekday_field(field: &str) -> Result<String> {
    // Asterisk and wildcard variants pass through unchanged.
    if field == "*" || field == "?" {
        return Ok(field.to_string());
    }

    // If the field contains any alphabetic character it uses named days
    // (e.g. MON-FRI) which the cron crate handles natively.
    if field.chars().any(|c| c.is_ascii_alphabetic()) {
        return Ok(field.to_string());
    }

    // The field may be a comma-separated list of items, where each item is
    // either a single value, a range (start-end), or a range/value with a
    // step (/N).
    let parts: Vec<&str> = field.split(',').collect();
    let mut result_parts = Vec::with_capacity(parts.len());

    for part in parts {
        // Split off optional step suffix first (e.g. "1-5/2" → "1-5" + "2").
        let (range_part, step) = if let Some((r, s)) = part.split_once('/') {
            (r, Some(s))
        } else {
            (part, None)
        };

        let translated = if let Some((start_s, end_s)) = range_part.split_once('-') {
            let start: u8 = start_s
                .parse()
                .with_context(|| format!("Invalid weekday in range: {start_s}"))?;
            let end: u8 = end_s
                .parse()
                .with_context(|| format!("Invalid weekday in range: {end_s}"))?;
            let new_start = translate_weekday_value(start)?;
            let new_end = translate_weekday_value(end)?;
            format!("{new_start}-{new_end}")
        } else if range_part == "*" {
            "*".to_string()
        } else {
            let val: u8 = range_part
                .parse()
                .with_context(|| format!("Invalid weekday value: {range_part}"))?;
            translate_weekday_value(val)?.to_string()
        };

        if let Some(s) = step {
            result_parts.push(format!("{translated}/{s}"));
        } else {
            result_parts.push(translated);
        }
    }

    Ok(result_parts.join(","))
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

    #[test]
    fn normalize_weekday_field_translates_standard_crontab_values() {
        // Single values: standard crontab → cron crate
        assert_eq!(normalize_weekday_field("0").unwrap(), "1"); // Sun
        assert_eq!(normalize_weekday_field("1").unwrap(), "2"); // Mon
        assert_eq!(normalize_weekday_field("5").unwrap(), "6"); // Fri
        assert_eq!(normalize_weekday_field("6").unwrap(), "7"); // Sat
        assert_eq!(normalize_weekday_field("7").unwrap(), "1"); // Sun (alias)
    }

    #[test]
    fn normalize_weekday_field_translates_ranges() {
        // 1-5 (Mon-Fri) → 2-6
        assert_eq!(normalize_weekday_field("1-5").unwrap(), "2-6");
        // 0-6 (Sun-Sat) → 1-7
        assert_eq!(normalize_weekday_field("0-6").unwrap(), "1-7");
    }

    #[test]
    fn normalize_weekday_field_translates_lists() {
        // 0,6 (Sun,Sat) → 1,7
        assert_eq!(normalize_weekday_field("0,6").unwrap(), "1,7");
        // 1,3,5 (Mon,Wed,Fri) → 2,4,6
        assert_eq!(normalize_weekday_field("1,3,5").unwrap(), "2,4,6");
    }

    #[test]
    fn normalize_weekday_field_translates_steps() {
        // 1-5/2 (Mon-Fri every other) → 2-6/2
        assert_eq!(normalize_weekday_field("1-5/2").unwrap(), "2-6/2");
        // */2 (every other day) → */2
        assert_eq!(normalize_weekday_field("*/2").unwrap(), "*/2");
    }

    #[test]
    fn normalize_weekday_field_passes_through_wildcards_and_names() {
        assert_eq!(normalize_weekday_field("*").unwrap(), "*");
        assert_eq!(normalize_weekday_field("?").unwrap(), "?");
        assert_eq!(normalize_weekday_field("MON-FRI").unwrap(), "MON-FRI");
        assert_eq!(
            normalize_weekday_field("MON,WED,FRI").unwrap(),
            "MON,WED,FRI"
        );
    }

    #[test]
    fn normalize_expression_applies_weekday_fix_to_5_field() {
        // "0 9 * * 1-5" should become "0 0 9 * * 2-6"
        let result = normalize_expression("0 9 * * 1-5").unwrap();
        assert_eq!(result, "0 0 9 * * 2-6");
    }

    #[test]
    fn normalize_expression_does_not_modify_6_field() {
        // 6-field expressions already use cron-crate semantics
        let result = normalize_expression("0 0 9 * * 1-5").unwrap();
        assert_eq!(result, "0 0 9 * * 1-5");
    }

    #[test]
    fn weekday_1_5_schedules_monday_through_friday() {
        // 2026-02-16 is a Monday. With "0 9 * * 1-5" (Mon-Fri at 09:00 UTC),
        // the next run from Sunday 2026-02-15 should be Monday 2026-02-16.
        let sunday = Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap();
        let schedule = Schedule::Cron {
            expr: "0 9 * * 1-5".into(),
            tz: None,
        };
        let next = next_run_for_schedule(&schedule, sunday).unwrap();
        // Should be Monday 2026-02-16 at 09:00 UTC (weekday = Mon)
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 2, 16, 9, 0, 0).unwrap());
        assert_eq!(next.weekday(), chrono::Weekday::Mon);
    }

    #[test]
    fn weekday_1_5_does_not_fire_on_saturday_or_sunday() {
        // From Friday evening, next run should skip Sat/Sun → Monday
        let friday_evening = Utc.with_ymd_and_hms(2026, 2, 20, 18, 0, 0).unwrap();
        let schedule = Schedule::Cron {
            expr: "0 9 * * 1-5".into(),
            tz: None,
        };
        let next = next_run_for_schedule(&schedule, friday_evening).unwrap();
        // Should be Monday 2026-02-23 at 09:00 UTC
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 2, 23, 9, 0, 0).unwrap());
        assert_eq!(next.weekday(), chrono::Weekday::Mon);
    }

    #[test]
    fn weekday_0_means_sunday() {
        // "0 10 * * 0" should fire on Sunday only
        let monday = Utc.with_ymd_and_hms(2026, 2, 16, 0, 0, 0).unwrap();
        let schedule = Schedule::Cron {
            expr: "0 10 * * 0".into(),
            tz: None,
        };
        let next = next_run_for_schedule(&schedule, monday).unwrap();
        assert_eq!(next.weekday(), chrono::Weekday::Sun);
    }

    #[test]
    fn weekday_7_means_sunday() {
        // "0 10 * * 7" should also fire on Sunday (alias)
        let monday = Utc.with_ymd_and_hms(2026, 2, 16, 0, 0, 0).unwrap();
        let schedule = Schedule::Cron {
            expr: "0 10 * * 7".into(),
            tz: None,
        };
        let next = next_run_for_schedule(&schedule, monday).unwrap();
        assert_eq!(next.weekday(), chrono::Weekday::Sun);
    }

    // ── Conditional schedule tests ────────────────────────────────

    use crate::cron::{ConditionType, ConditionalSchedule};

    fn make_conditional(
        expr: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> ConditionalSchedule {
        ConditionalSchedule {
            schedule: Schedule::Cron {
                expr: expr.to_string(),
                tz: None,
            },
            condition_type: ConditionType::DateRange,
            condition_start: start,
            condition_end: end,
        }
    }

    #[test]
    fn effective_next_run_uses_primary_when_no_conditional() {
        let from = Utc.with_ymd_and_hms(2026, 3, 28, 12, 0, 0).unwrap();
        let primary = Schedule::Cron {
            expr: "0 */6 * * *".into(),
            tz: None,
        };
        let next = effective_next_run(&primary, None, from).unwrap();
        // Next 6-hourly slot after 12:00 is 18:00
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 3, 28, 18, 0, 0).unwrap());
    }

    #[test]
    fn effective_next_run_uses_conditional_when_active() {
        let from = Utc.with_ymd_and_hms(2026, 3, 29, 10, 0, 0).unwrap();
        let primary = Schedule::Cron {
            expr: "0 */6 * * *".into(),
            tz: None,
        };
        let cond = make_conditional(
            "*/30 * * * *",
            Utc.with_ymd_and_hms(2026, 3, 28, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 3, 31, 23, 59, 59).unwrap(),
        );
        let next = effective_next_run(&primary, Some(&cond), from).unwrap();
        // Should use 30-min schedule: next after 10:00 is 10:30
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 3, 29, 10, 30, 0).unwrap());
    }

    #[test]
    fn effective_next_run_uses_primary_when_condition_inactive() {
        let from = Utc.with_ymd_and_hms(2026, 4, 5, 10, 0, 0).unwrap();
        let primary = Schedule::Cron {
            expr: "0 */6 * * *".into(),
            tz: None,
        };
        let cond = make_conditional(
            "*/30 * * * *",
            Utc.with_ymd_and_hms(2026, 3, 28, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 3, 31, 23, 59, 59).unwrap(),
        );
        let next = effective_next_run(&primary, Some(&cond), from).unwrap();
        // Condition ended on March 31, so primary schedule applies
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).unwrap());
    }

    #[test]
    fn effective_next_run_falls_back_when_conditional_exceeds_window() {
        // Condition ends soon but conditional schedule is infrequent
        let from = Utc.with_ymd_and_hms(2026, 3, 31, 23, 50, 0).unwrap();
        let primary = Schedule::Cron {
            expr: "0 */6 * * *".into(),
            tz: None,
        };
        // Conditional is every 6 hours too, but condition ends at 23:59:59
        let cond = make_conditional(
            "0 */6 * * *",
            Utc.with_ymd_and_hms(2026, 3, 28, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 3, 31, 23, 59, 59).unwrap(),
        );
        let next = effective_next_run(&primary, Some(&cond), from).unwrap();
        // The conditional schedule's next run (06:00 April 1) exceeds the
        // condition window end, so we fall back to the primary schedule.
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn conditional_schedule_is_active_at_boundaries() {
        let start = Utc.with_ymd_and_hms(2026, 3, 28, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 3, 31, 23, 59, 59).unwrap();
        let cond = make_conditional("*/30 * * * *", start, end);

        // Exactly at start
        assert!(cond.is_active(start));
        // Exactly at end
        assert!(cond.is_active(end));
        // One second before start
        assert!(!cond.is_active(start - ChronoDuration::seconds(1)));
        // One second after end
        assert!(!cond.is_active(end + ChronoDuration::seconds(1)));
    }

    #[test]
    fn condition_type_parses_date_range() {
        assert_eq!(
            "date_range".parse::<ConditionType>().unwrap(),
            ConditionType::DateRange
        );
        assert_eq!(
            "DATE_RANGE".parse::<ConditionType>().unwrap(),
            ConditionType::DateRange
        );
        assert!("unknown".parse::<ConditionType>().is_err());
    }
}
