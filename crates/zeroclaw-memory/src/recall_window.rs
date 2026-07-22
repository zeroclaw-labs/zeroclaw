//! Shared parsing of the optional RFC 3339 `since`/`until` recall window.
//!
//! The markdown backend and the enrichment wrapper accept the same window
//! arguments; this is the single implementation of bound parsing and
//! ordering validation so their error contracts cannot drift.

pub(crate) type RecallTimestamp = chrono::DateTime<chrono::FixedOffset>;
pub(crate) type RecallWindow = (Option<RecallTimestamp>, Option<RecallTimestamp>);

fn parse_bound(
    value: Option<&str>,
    field: &'static str,
) -> anyhow::Result<Option<RecallTimestamp>> {
    value
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|error| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "field": field,
                        "error": error.to_string()
                    })),
                "recall window bound rejected"
            );
            anyhow::Error::msg(format!(
                "invalid '{field}' date (expected RFC 3339): {error}"
            ))
        })
}

/// Parse and validate an optional `[since, until]` recall window.
pub(crate) fn parse_recall_window(
    since: Option<&str>,
    until: Option<&str>,
) -> anyhow::Result<RecallWindow> {
    let since_dt = parse_bound(since, "since")?;
    let until_dt = parse_bound(until, "until")?;

    if let (Some(since), Some(until)) = (&since_dt, &until_dt)
        && since >= until
    {
        anyhow::bail!("'since' must be before 'until'");
    }

    Ok((since_dt, until_dt))
}
