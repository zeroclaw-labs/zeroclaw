//! Resolve a `/load-session` query against the sessions a pane can see.
//!
//! Pure and I/O-free: the caller supplies the pane-scoped list (see
//! `Chat::list_pane_sessions`) and decides what to do with the result.

use crate::client::SessionEntry;

/// Outcome of matching a `/load-session` query.
#[derive(Debug, Clone)]
pub(crate) enum SessionMatch {
    /// Exactly one session matched in the first tier that had any hit.
    One(SessionEntry),
    /// Several sessions matched in the same tier, in input (store) order.
    Many(Vec<SessionEntry>),
    /// No tier matched.
    NoMatch,
}

/// Match `query` against `sessions`, checking tiers in order and stopping at
/// the first tier with at least one hit:
///
/// 1. exact `session_id`
/// 2. exact `name`
/// 3. case-insensitive `name` (Unicode lowercase, not full case folding)
/// 4. case-insensitive `session_id` prefix
///
/// An empty query returns [`SessionMatch::NoMatch`] without checking any tier.
pub(crate) fn resolve_session(sessions: &[SessionEntry], query: &str) -> SessionMatch {
    // The parser routes an empty argument to the picker; guard anyway so an
    // empty prefix can never match every session.
    if query.is_empty() {
        return SessionMatch::NoMatch;
    }
    let lowered = query.to_lowercase();
    let tiers: [&dyn Fn(&SessionEntry) -> bool; 4] = [
        &|s| s.session_id == query,
        &|s| s.name.as_deref() == Some(query),
        &|s| {
            s.name
                .as_deref()
                .is_some_and(|n| n.to_lowercase() == lowered)
        },
        &|s| s.session_id.to_lowercase().starts_with(&lowered),
    ];
    for matches in tiers {
        let mut hits: Vec<SessionEntry> = sessions.iter().filter(|s| matches(s)).cloned().collect();
        match hits.len() {
            0 => continue,
            1 => return SessionMatch::One(hits.remove(0)),
            _ => return SessionMatch::Many(hits),
        }
    }
    SessionMatch::NoMatch
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, name: Option<&str>) -> SessionEntry {
        SessionEntry {
            session_id: id.to_string(),
            session_key: id.to_string(),
            created_at: "2026-10-27T00:00:00Z".to_string(),
            last_activity: "2026-10-27T00:05:00Z".to_string(),
            message_count: 0,
            agent_alias: Some("alpha".to_string()),
            channel_id: None,
            name: name.map(str::to_string),
        }
    }

    fn one_id(result: SessionMatch) -> String {
        match result {
            SessionMatch::One(entry) => entry.session_id,
            other => panic!("expected One, got {other:?}"),
        }
    }

    fn many_ids(result: SessionMatch) -> Vec<String> {
        match result {
            SessionMatch::Many(entries) => entries.into_iter().map(|e| e.session_id).collect(),
            other => panic!("expected Many, got {other:?}"),
        }
    }

    #[test]
    fn exact_id_beats_a_longer_id_sharing_the_prefix() {
        let sessions = [entry("abc-1", None), entry("abc-12", None)];
        assert_eq!(one_id(resolve_session(&sessions, "abc-1")), "abc-1");
    }

    #[test]
    fn exact_id_beats_a_session_named_like_the_query() {
        let sessions = [entry("other", Some("x")), entry("x", None)];
        assert_eq!(one_id(resolve_session(&sessions, "x")), "x");
    }

    #[test]
    fn exact_name_beats_a_case_insensitive_name() {
        let sessions = [
            entry("s-lower", Some("deploy")),
            entry("s-exact", Some("Deploy")),
        ];
        assert_eq!(one_id(resolve_session(&sessions, "Deploy")), "s-exact");
    }

    #[test]
    fn case_insensitive_name_matches_and_keeps_inner_spaces() {
        let sessions = [
            entry("s-1", Some("Deploy Fix")),
            entry("s-2", Some("Other")),
        ];
        assert_eq!(one_id(resolve_session(&sessions, "deploy fix")), "s-1");
    }

    #[test]
    fn ambiguous_case_insensitive_names_return_many_in_store_order() {
        let sessions = [
            entry("s-2", Some("DEPLOY")),
            entry("s-9", Some("unrelated")),
            entry("s-1", Some("Deploy")),
        ];
        assert_eq!(
            many_ids(resolve_session(&sessions, "deploy")),
            ["s-2", "s-1"]
        );
    }

    #[test]
    fn unique_id_prefix_matches_case_insensitively() {
        let sessions = [entry("9F3A0000-aaaa", None), entry("1b2c0000-bbbb", None)];
        assert_eq!(one_id(resolve_session(&sessions, "9f3a")), "9F3A0000-aaaa");
    }

    #[test]
    fn ambiguous_id_prefix_returns_many_in_store_order() {
        let sessions = [entry("sess-dup-2", None), entry("sess-dup-1", None)];
        assert_eq!(
            many_ids(resolve_session(&sessions, "sess-dup")),
            ["sess-dup-2", "sess-dup-1"]
        );
    }

    #[test]
    fn unnamed_sessions_do_not_match_by_name() {
        let sessions = [entry("s-1", None), entry("s-2", Some("foo"))];
        assert_eq!(one_id(resolve_session(&sessions, "foo")), "s-2");
    }

    #[test]
    fn case_insensitive_name_beats_an_id_prefix() {
        let sessions = [entry("cafe1234-0000", None), entry("s-named", Some("Cafe"))];
        assert_eq!(one_id(resolve_session(&sessions, "cafe")), "s-named");
    }

    #[test]
    fn exact_name_beats_an_id_prefix() {
        let sessions = [entry("cafe1234-0000", None), entry("s-named", Some("cafe"))];
        assert_eq!(one_id(resolve_session(&sessions, "cafe")), "s-named");
    }

    #[test]
    fn empty_list_is_no_match() {
        assert!(matches!(
            resolve_session(&[], "anything"),
            SessionMatch::NoMatch
        ));
    }

    #[test]
    fn empty_query_is_no_match_rather_than_every_prefix() {
        let sessions = [entry("s-1", None), entry("s-2", None)];
        assert!(matches!(
            resolve_session(&sessions, ""),
            SessionMatch::NoMatch
        ));
    }
}
