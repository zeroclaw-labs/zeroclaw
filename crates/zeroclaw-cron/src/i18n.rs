//! Operator-facing text for cron results.
//!
//! Cron carries its own catalogue rather than reaching into the runtime's:
//! this crate does not depend on the runtime, and the strings it renders are
//! its own. The helper names match the runtime's so the calling code reads the
//! same either side of the extraction.
//!
//! Machine-readable run statuses are deliberately absent. `skipped_precondition`,
//! `precondition_failed`, and `already_in_flight` are a wire contract shared
//! with the API, the tools, and stored history; they must not vary by locale.

use fluent::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use unic_langid::LanguageIdentifier;

/// English catalogue, compiled in so a missing locale directory degrades to
/// readable text rather than to key names.
const EN_CRON_FTL: &str = include_str!("../locales/en/cron.ftl");

/// Build a bundle for one lookup.
///
/// `FluentBundle` holds a non-`Sync` memoizer, so it cannot be cached in a
/// static. The runtime catalogue has the same constraint and resolves it the
/// same way. Cron renders these strings once per run outcome, not in a hot
/// path, so rebuilding is not worth working around.
fn bundle() -> Option<FluentBundle<FluentResource>> {
    let langid: LanguageIdentifier = "en".parse().ok()?;
    let mut bundle = FluentBundle::new(vec![langid]);
    // Directional isolates would wrap every interpolated value in characters
    // callers then have to strip before comparing. The runtime catalogue
    // disables them for the same reason.
    bundle.set_use_isolating(false);
    let resource = FluentResource::try_new(EN_CRON_FTL.to_string()).ok()?;
    bundle.add_resource(resource).ok()?;
    Some(bundle)
}

fn format(key: &str, args: Option<&FluentArgs<'_>>) -> Option<String> {
    let bundle = bundle()?;
    let message = bundle.get_message(key)?;
    let pattern = message.value()?;
    let mut errors = Vec::new();
    let rendered = bundle.format_pattern(pattern, args, &mut errors);
    // A message that references an argument it was not given renders with a
    // placeholder and reports an error. Treat that as a miss so the caller
    // sees the key rather than half-formatted text.
    if errors.is_empty() {
        Some(rendered.into_owned())
    } else {
        None
    }
}

/// Render `key`, falling back to the key itself when it is missing.
///
/// A missing key is a catalogue bug, not a runtime failure: returning the key
/// keeps the surrounding result usable and makes the gap visible in output and
/// in tests.
#[must_use]
pub fn get_required_cli_string(key: &str) -> String {
    format(key, None).unwrap_or_else(|| format!("{{{key}}}"))
}

/// Render `key` with Fluent external arguments.
#[must_use]
pub fn get_required_cli_string_with_args(key: &str, args: &[(&str, &str)]) -> String {
    let mut fluent_args = FluentArgs::new();
    for (name, value) in args {
        fluent_args.set(*name, FluentValue::from(*value));
    }
    format(key, Some(&fluent_args)).unwrap_or_else(|| format!("{{{key}}}"))
}
