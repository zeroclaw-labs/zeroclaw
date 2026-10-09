//! Wall-clock context for the transcript: a divider before a prompt that
//! follows a gap or a day change, and a footer after a turn that ran long.
//! Individual messages carry no time on screen.

use chrono::{DateTime, Local, NaiveDate, TimeDelta};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme;

/// A prompt this long after the previous activity gets a divider.
const DIVIDER_GAP_SECS: i64 = 15 * 60;
/// A turn at least this long gets a footer.
const FOOTER_MIN_SECS: i64 = 60;

/// Time facts kept per transcript entry, index-aligned with the entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct EntryStamp {
    /// When the entry happened: the daemon's `created_at` for history, the
    /// local receive time for live entries.
    pub at: Option<DateTime<Local>>,
    /// Whether this entry is the last output of a finished turn; the turn's
    /// footer follows it and its `at` is the turn's end.
    pub ends_turn: TurnEnd,
}

/// How an entry came to end its turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum TurnEnd {
    #[default]
    No,
    /// Inferred from a history load, which may have been taken mid-turn; a
    /// live settle of the same turn replaces it.
    Loaded,
    /// Recorded when the turn settled live; later frames do not move it.
    Settled,
}

/// What to draw around one entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TimeMarks {
    /// Divider before the entry, at this time.
    pub divider: Option<DateTime<Local>>,
    /// Footer after the entry: the turn's `(start, end)`.
    pub footer: Option<(DateTime<Local>, DateTime<Local>)>,
}

/// Decide dividers and footers for a transcript. `is_user[i]` marks the
/// entries that start a turn.
pub(super) fn time_marks(is_user: &[bool], stamps: &[EntryStamp]) -> Vec<TimeMarks> {
    let stamp = |index: usize| stamps.get(index).copied().unwrap_or_default();
    let mut marks = vec![TimeMarks::default(); is_user.len()];

    let mut latest: Option<DateTime<Local>> = None;
    for (index, &user) in is_user.iter().enumerate() {
        let at = stamp(index).at;
        if user && let Some(at) = at {
            let show = latest.is_none_or(|previous| {
                previous.date_naive() != at.date_naive()
                    || (at - previous).num_seconds() >= DIVIDER_GAP_SECS
            });
            if show {
                marks[index].divider = Some(at);
            }
        }
        if let Some(at) = at {
            latest = Some(latest.map_or(at, |previous| previous.max(at)));
        }
    }

    let mut start_index = 0;
    while start_index < is_user.len() {
        if !is_user[start_index] {
            start_index += 1;
            continue;
        }
        let mut next = start_index + 1;
        let mut end_index = None;
        while next < is_user.len() && !is_user[next] {
            if stamp(next).ends_turn != TurnEnd::No {
                end_index = Some(next);
            }
            next += 1;
        }
        if let (Some(start), Some(end_index)) = (stamp(start_index).at, end_index)
            && let Some(end) = stamp(end_index).at
            && shows_footer(start, end)
        {
            marks[end_index].footer = Some((start, end));
        }
        start_index = next;
    }
    marks
}

/// Whether a turn from `start` to `end` is long enough for a footer.
pub(super) fn shows_footer(start: DateTime<Local>, end: DateTime<Local>) -> bool {
    (end - start).num_seconds() >= FOOTER_MIN_SECS
}

/// Parse a daemon `created_at` (RFC 3339) into local time. A value that does
/// not parse yields no time.
pub(super) fn parse_created_at(value: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.with_timezone(&Local))
}

fn clock(at: &DateTime<Local>) -> String {
    at.format("%H:%M").to_string()
}

fn dated(at: &DateTime<Local>) -> String {
    at.format("%Y-%m-%d %H:%M").to_string()
}

/// `Today 01:00`, `Yesterday 22:20`, or `2026-10-05 14:00`.
pub(super) fn divider_label(at: &DateTime<Local>, today: NaiveDate) -> String {
    let time = clock(at);
    if at.date_naive() == today {
        crate::i18n::t_args("zc-chat-time-today", &[("time", &time)])
    } else if today.pred_opt() == Some(at.date_naive()) {
        crate::i18n::t_args("zc-chat-time-yesterday", &[("time", &time)])
    } else {
        dated(at)
    }
}

/// `01:00 → 01:31 · 31m 40s`. The start is dated when it is not today; the
/// end is dated when it falls on another day than the start.
pub(super) fn footer_label(
    start: &DateTime<Local>,
    end: &DateTime<Local>,
    today: NaiveDate,
) -> String {
    let start_label = if start.date_naive() == today {
        clock(start)
    } else {
        dated(start)
    };
    let end_label = if end.date_naive() == start.date_naive() {
        clock(end)
    } else {
        dated(end)
    };
    crate::i18n::t_args(
        "zc-chat-turn-span",
        &[
            ("start", &start_label),
            ("end", &end_label),
            ("duration", &duration_label(*end - *start)),
        ],
    )
}

/// `45s`, `31m 40s`, `2h 05m`.
pub(super) fn duration_label(duration: TimeDelta) -> String {
    let secs = duration.num_seconds().max(0);
    let (hours, minutes, seconds) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if hours > 0 {
        crate::i18n::t_args(
            "zc-chat-duration-hours-minutes",
            &[
                ("hours", &hours.to_string()),
                ("minutes", &format!("{minutes:02}")),
            ],
        )
    } else if minutes > 0 {
        crate::i18n::t_args(
            "zc-chat-duration-minutes-seconds",
            &[
                ("minutes", &minutes.to_string()),
                ("seconds", &format!("{seconds:02}")),
            ],
        )
    } else {
        crate::i18n::t_args(
            "zc-chat-duration-seconds",
            &[("seconds", &seconds.to_string())],
        )
    }
}

/// Centered dim divider row.
pub(super) fn divider_line(label: &str, width: u16) -> Line<'static> {
    let text = format!("── {label} ──");
    let pad = usize::from(width).saturating_sub(text.width()) / 2;
    Line::from(Span::styled(
        format!("{}{text}", " ".repeat(pad)),
        theme::dim_style(),
    ))
}

/// Dim footer row under a turn.
pub(super) fn footer_line(label: &str) -> Line<'static> {
    Line::from(Span::styled(format!("─── {label}"), theme::dim_style()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn local(day: u32, hour: u32, minute: u32, second: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 10, day, hour, minute, second)
            .single()
            .expect("unambiguous local time")
    }

    fn stamp(at: DateTime<Local>) -> EntryStamp {
        EntryStamp {
            at: Some(at),
            ends_turn: TurnEnd::No,
        }
    }

    fn ending(at: DateTime<Local>) -> EntryStamp {
        EntryStamp {
            at: Some(at),
            ends_turn: TurnEnd::Settled,
        }
    }

    #[test]
    fn dividers_mark_the_first_prompt_gaps_and_day_changes_only() {
        // user, agent, user (+2 min), agent, user (+20 min), agent, user (next day)
        let is_user = [true, false, true, false, true, false, true];
        let stamps = [
            stamp(local(8, 22, 20, 0)),
            ending(local(8, 22, 21, 0)),
            stamp(local(8, 22, 23, 0)),
            ending(local(8, 22, 24, 0)),
            stamp(local(8, 22, 44, 0)),
            ending(local(8, 22, 45, 0)),
            stamp(local(9, 0, 1, 0)),
        ];
        let dividers: Vec<_> = time_marks(&is_user, &stamps)
            .iter()
            .map(|mark| mark.divider.is_some())
            .collect();
        assert_eq!(
            dividers,
            [true, false, false, false, true, false, true],
            "first prompt, the 20-minute gap and the day change; not the 2-minute follow-up"
        );
    }

    #[test]
    fn a_day_change_divides_even_after_a_short_gap() {
        let is_user = [true, false, true];
        let stamps = [
            stamp(local(8, 23, 58, 0)),
            ending(local(8, 23, 59, 0)),
            stamp(local(9, 0, 1, 0)),
        ];
        let marks = time_marks(&is_user, &stamps);
        assert_eq!(
            marks[2].divider,
            Some(local(9, 0, 1, 0)),
            "two minutes, but a new day"
        );
    }

    #[test]
    fn footers_follow_finished_turns_of_a_minute_or_more() {
        let is_user = [true, false, false, true, false, true, false];
        let stamps = [
            stamp(local(9, 1, 0, 0)),
            stamp(local(9, 1, 31, 40)),
            ending(local(9, 1, 31, 40)),
            stamp(local(9, 1, 40, 0)),
            ending(local(9, 1, 40, 30)),
            stamp(local(9, 2, 0, 0)),
            // Still running: no entry ends the turn yet.
            stamp(local(9, 2, 30, 0)),
        ];
        let footers: Vec<_> = time_marks(&is_user, &stamps)
            .iter()
            .map(|mark| mark.footer)
            .collect();
        assert_eq!(footers[2], Some((local(9, 1, 0, 0), local(9, 1, 31, 40))));
        assert_eq!(footers[4], None, "a 30-second turn gets no footer");
        assert_eq!(footers[6], None, "a running turn gets no footer");
        assert_eq!(footers.iter().flatten().count(), 1);
    }

    #[test]
    fn untimed_history_gets_no_marks() {
        let marks = time_marks(&[true, false, true], &[EntryStamp::default(); 3]);
        assert!(marks.iter().all(|mark| *mark == TimeMarks::default()));
    }

    #[test]
    fn labels_read_today_yesterday_or_a_date() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        assert_eq!(divider_label(&local(9, 1, 0, 0), today), "Today 01:00");
        assert_eq!(
            divider_label(&local(8, 22, 20, 0), today),
            "Yesterday 22:20"
        );
        assert_eq!(
            divider_label(&local(5, 14, 0, 0), today),
            "2026-10-05 14:00"
        );
        assert_eq!(
            footer_label(&local(9, 1, 0, 0), &local(9, 1, 31, 40), today),
            "01:00 → 01:31 · 31m 40s"
        );
        assert_eq!(
            footer_label(&local(8, 23, 50, 0), &local(9, 0, 20, 0), today),
            "2026-10-08 23:50 → 2026-10-09 00:20 · 30m 00s"
        );
        assert_eq!(duration_label(TimeDelta::seconds(45)), "45s");
        assert_eq!(
            duration_label(TimeDelta::seconds(2 * 3600 + 5 * 60)),
            "2h 05m"
        );
    }

    #[test]
    fn parse_created_at_accepts_rfc3339_and_rejects_garbage() {
        let parsed = parse_created_at("2026-10-08T17:46:07.123456+00:00").unwrap();
        assert_eq!(
            parsed.with_timezone(&chrono::Utc).to_rfc3339(),
            "2026-10-08T17:46:07.123456+00:00"
        );
        assert!(parse_created_at("yesterday-ish").is_none());
        assert!(parse_created_at("").is_none());
    }
}
