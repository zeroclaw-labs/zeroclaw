//! Shared `allowed_users` matching used by every chat channel.

/// Case-sensitivity selector for the allowlist comparison. The chat
/// platform defines which one applies; the helper does not infer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// Exact `==` match.
    Sensitive,
    /// `eq_ignore_ascii_case` — IRC nicks, Matrix MXIDs.
    CaseInsensitive,
}

/// Marks an entry as a deny rule rather than a grant.
///
/// `Config::channel_external_peers` emits `!<name>` for every `ignore` entry and
/// applies none of them itself, because only the channel knows whether two
/// spellings name the same account. Re-exported from the producing crate so the
/// two halves of the contract cannot drift.
pub use zeroclaw_config::schema::PEER_DENY_PREFIX as DENY_PREFIX;

/// Whether a resolved peer list can admit anybody at all.
///
/// "Is this list empty" and "has anybody been authorized" are different
/// questions, because the list also carries a `!name` deny for every `ignore`
/// entry. Code that gates pairing or explains an unauthorized sender wants this
/// one.
///
/// A grant only counts when a deny does not already shadow it, so
/// `["alice", "!alice"]` and `["*", "!*"]` both answer `false`: they are
/// syntactic grants that admit nobody, and reporting them as authorization
/// leaves the operator with no accepted sender and no pairing route back. Only
/// the channel's own matcher can decide the general case, so this answers
/// conservatively with the normalized comparison a deny is matched by anyway —
/// erring toward "nobody is authorized" is what keeps recovery pairing
/// available, and the sender that then arrives is still judged by
/// `is_identity_allowed_by`.
#[must_use]
pub fn grants_anyone(allowed: &[String]) -> bool {
    let denies: Vec<&str> = allowed
        .iter()
        .filter_map(|entry| entry.strip_prefix(DENY_PREFIX))
        .collect();
    // `ignore = ["*"]` denies every sender, so no grant survives it.
    if denies.iter().any(|entry| is_wildcard(entry)) {
        return false;
    }
    allowed
        .iter()
        .filter(|entry| !entry.starts_with(DENY_PREFIX))
        .any(|grant| {
            // A wildcard grant outlives any named deny: every sender the denies
            // do not name still rides it.
            is_wildcard(grant)
                || !denies
                    .iter()
                    .any(|deny| deny_names(deny, grant, &|entry: &str, user: &str| entry == user))
        })
}

/// The conflict message for a pairing write an `ignore` would shadow, or
/// `None` when the write can proceed.
///
/// Pairing has to end in an identity the admission matcher accepts. When an
/// explicit deny names the identity, appending a grant persists something the
/// deny immediately shadows: the operator is told the account is bound and it
/// still cannot talk, and `grants_anyone` keeps offering the same prompt. Every
/// writer of a paired identity asks this first so the `ignore` stays
/// authoritative and the operator gets told what to edit.
///
/// The message names the config location and never the identity: identities are
/// durable personal identifiers and callers log this error.
#[must_use]
pub fn pairing_deny_conflict(
    allowed: &[String],
    channel_type: &str,
    alias: &str,
    identity: &str,
    match_fn: impl Fn(&str, &str) -> bool,
) -> Option<String> {
    is_user_denied(allowed, identity, &match_fn).then(|| {
        format!(
            "paired {channel_type}.{alias} identity is denied by an `ignore` entry in a \
             matching [peer_groups.*] block — remove that entry from config.toml, then \
             pair again"
        )
    })
}

/// Whether a grant entry admits every sender.
///
/// Trimmed, because an operator writing `external_peers = [" * "]` means the
/// wildcard, and a channel matcher that trims would honour it as one while an
/// exact comparison here would not.
fn is_wildcard(entry: &str) -> bool {
    entry.trim() == "*"
}

/// Whether a deny entry names `user`.
///
/// A deny rule is checked with the caller's own notion of identity *and* with a
/// normalized comparison (leading `@` stripped, ASCII case-insensitive). A
/// blocklist errs toward denying, so matching a superset is the safe direction:
/// the alternative admits a sender the operator wrote down.
fn deny_names(entry: &str, user: &str, match_fn: &impl Fn(&str, &str) -> bool) -> bool {
    // `ignore = ["*"]` denies every sender, the mirror of a wildcard grant.
    if is_wildcard(entry) {
        return true;
    }
    let normalize = |value: &str| value.trim().trim_start_matches('@').to_string();
    match_fn(entry, user) || normalize(entry).eq_ignore_ascii_case(&normalize(user))
}

/// Whether any deny entry in `allowed` names `user`.
///
/// Evaluated before the wildcard so an explicit deny always wins.
fn is_user_denied(allowed: &[String], user: &str, match_fn: &impl Fn(&str, &str) -> bool) -> bool {
    allowed
        .iter()
        .filter_map(|entry| entry.strip_prefix(DENY_PREFIX))
        .any(|entry| deny_names(entry, user, match_fn))
}

fn matcher_for(mode: Match) -> impl Fn(&str, &str) -> bool {
    move |entry: &str, user: &str| match mode {
        Match::Sensitive => entry == user,
        Match::CaseInsensitive => entry.eq_ignore_ascii_case(user),
    }
}

/// Whether any identifier of one account is explicitly denied, independent of
/// any grant.
///
/// Needed by channels that accept more than one identifier for the same
/// account. Asking `is_user_allowed_by` per identifier is not sufficient
/// there: a deny on one alias is defeated by a wildcard grant reached through
/// another, so the sender has to be rejected when *any* of its identifiers is
/// denied.
#[must_use]
pub fn is_identity_denied_by(
    allowed: &[String],
    identities: &[&str],
    match_fn: impl Fn(&str, &str) -> bool,
) -> bool {
    identities
        .iter()
        .any(|user| is_user_denied(allowed, user, &match_fn))
}

/// Whether an account is authorized, evaluated across every identifier it is
/// known by, against a single snapshot of the resolved peer list.
///
/// Asking `is_user_allowed_by` once per identifier and OR-ing the answers is
/// not equivalent. A deny names one identifier while the wildcard grants every
/// other, so the deny goes false on its own identifier and the wildcard goes
/// true on the next one, and the account is admitted. Any channel that accepts
/// more than one identifier for the same sender, or that resolves the peer list
/// more than once while deciding, has that hole.
#[must_use]
pub fn is_identity_allowed_by(
    allowed: &[String],
    identities: &[&str],
    match_fn: impl Fn(&str, &str) -> bool,
) -> bool {
    // An account the channel could not identify is not authorized, wildcard or
    // not: there is nothing for a deny rule to name.
    if identities.is_empty() {
        return false;
    }
    if is_identity_denied_by(allowed, identities, &match_fn) {
        return false;
    }
    let grants = || {
        allowed
            .iter()
            .filter(|entry| !entry.starts_with(DENY_PREFIX))
    };
    if grants().any(|entry| is_wildcard(entry)) {
        return true;
    }
    grants().any(|entry| identities.iter().any(|user| match_fn(entry, user)))
}

/// `is_identity_allowed_by` with the shared case-sensitivity selector.
#[must_use]
pub fn is_identity_allowed(allowed: &[String], identities: &[&str], mode: Match) -> bool {
    is_identity_allowed_by(allowed, identities, matcher_for(mode))
}

#[must_use]
pub fn is_user_allowed(allowed: &[String], user: &str, mode: Match) -> bool {
    is_identity_allowed_by(allowed, &[user], matcher_for(mode))
}

#[must_use]
pub fn is_user_allowed_by(
    allowed: &[String],
    user: &str,
    match_fn: impl Fn(&str, &str) -> bool,
) -> bool {
    is_identity_allowed_by(allowed, &[user], match_fn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_allows_anyone() {
        let list = vec!["*".to_string()];
        assert!(is_user_allowed(&list, "alice", Match::Sensitive));
        assert!(is_user_allowed(&list, "ALICE", Match::Sensitive));
    }

    #[test]
    fn deny_marker_overrides_wildcard() {
        let list = vec!["*".to_string(), "!alice".to_string()];
        assert!(!is_user_allowed(&list, "alice", Match::Sensitive));
        // Everyone else still rides the wildcard.
        assert!(is_user_allowed(&list, "bob", Match::Sensitive));
    }

    #[test]
    fn deny_marker_overrides_an_explicit_grant() {
        // The shape `channel_external_peers` produces for an explicit grant that
        // another group ignores. Nothing is subtracted there, so this precedence
        // is what makes the deny effective.
        let list = vec!["alice".to_string(), "!alice".to_string()];
        assert!(!is_user_allowed(&list, "alice", Match::Sensitive));
    }

    #[test]
    fn deny_marker_is_not_itself_a_grant() {
        // Without the filter, `!alice` would authorize a sender literally
        // named `!alice`.
        let list = vec!["!alice".to_string()];
        assert!(!is_user_allowed(&list, "!alice", Match::Sensitive));
        assert!(!is_user_allowed(&list, "alice", Match::Sensitive));
    }

    #[test]
    fn deny_marker_ignores_case_and_a_leading_at() {
        // A blocklist errs toward denying, so a deny is matched more broadly
        // than a grant: on a case-sensitive channel `ignore = ["Alice"]` still
        // denies `alice`.
        let list = vec!["*".to_string(), "!@Alice".to_string()];
        assert!(!is_user_allowed(&list, "alice", Match::Sensitive));
        assert!(!is_user_allowed(&list, "@ALICE", Match::Sensitive));
    }

    #[test]
    fn grants_anyone_ignores_deny_markers() {
        // A list of nothing but denies has authorized no one, so callers that
        // treated a non-empty list as "somebody is paired" need this instead.
        assert!(!grants_anyone(&["!alice".to_string()]));
        assert!(!grants_anyone(&[]));
        assert!(grants_anyone(&["*".to_string(), "!alice".to_string()]));
        assert!(grants_anyone(&["alice".to_string()]));
    }

    #[test]
    fn grants_anyone_rejects_a_grant_its_own_deny_shadows() {
        // A grant cancelled by a deny admits nobody. Reporting it as
        // authorization is what suppresses the pairing prompt, so the operator
        // is left with no accepted sender and no way back in.
        for shadowed in [
            vec!["alice".to_string(), "!alice".to_string()],
            // Broader deny matching applies here too, or a differently spelled
            // ignore would look like a live grant.
            vec!["alice".to_string(), "!@ALICE".to_string()],
        ] {
            assert!(!grants_anyone(&shadowed));
            assert!(!is_user_allowed(&shadowed, "alice", Match::Sensitive));
        }

        // One named deny does not shadow a wildcard: every other sender is
        // still admitted, so pairing correctly stays suppressed.
        let wildcard = vec!["*".to_string(), "!alice".to_string()];
        assert!(grants_anyone(&wildcard));
        assert!(is_user_allowed(&wildcard, "bob", Match::Sensitive));

        // A wildcard deny shadows every grant, wildcard or named.
        assert!(!grants_anyone(&["*".to_string(), "!*".to_string()]));
        assert!(!grants_anyone(&["alice".to_string(), "!*".to_string()]));

        // A surviving grant alongside a shadowed one still counts.
        let partial = vec!["alice".to_string(), "bob".to_string(), "!alice".to_string()];
        assert!(grants_anyone(&partial));
        assert!(is_user_allowed(&partial, "bob", Match::Sensitive));
    }

    #[test]
    fn pairing_deny_conflict_reports_the_config_location_not_the_identity() {
        let denied = vec!["*".to_string(), "!+15551234567".to_string()];
        let conflict =
            pairing_deny_conflict(&denied, "whatsapp", "admin", "+15551234567", |a, b| a == b)
                .expect("an ignored identity must be reported as a conflict");
        assert!(conflict.contains("whatsapp.admin"));
        assert!(conflict.contains("ignore"));
        assert!(
            !conflict.contains("+15551234567"),
            "identities are personal data and callers log this message: {conflict}"
        );

        // Nothing to report when the write would actually take effect.
        assert!(
            pairing_deny_conflict(&denied, "whatsapp", "admin", "+15559999999", |a, b| a == b)
                .is_none()
        );
        assert!(
            pairing_deny_conflict(&[], "whatsapp", "admin", "+15551234567", |a, b| a == b)
                .is_none()
        );
    }

    #[test]
    fn wildcard_deny_denies_everyone() {
        // `ignore = ["*"]` is the mirror of a wildcard grant. Removing the
        // wildcard grant by subtraction used to achieve this; now the deny
        // itself has to say it.
        let list = vec!["*".to_string(), "!*".to_string()];
        assert!(!is_user_allowed(&list, "alice", Match::Sensitive));
        assert!(!is_user_allowed(&list, "bob", Match::Sensitive));
    }

    #[test]
    fn wildcard_deny_outranks_an_explicit_grant() {
        let list = vec!["alice".to_string(), "!*".to_string()];
        assert!(!is_user_allowed(&list, "alice", Match::Sensitive));
    }

    #[test]
    fn padded_wildcard_grant_is_a_wildcard() {
        // Channels that trim before matching read `" * "` as the wildcard. The
        // shared helper has to agree, or a deny is evaluated against a list this
        // helper thinks grants nobody while the channel thinks it grants all.
        let list = vec![" * ".to_string()];
        assert!(is_user_allowed(&list, "alice", Match::Sensitive));

        let denied = vec![" * ".to_string(), "!alice".to_string()];
        assert!(!is_user_allowed(&denied, "alice", Match::Sensitive));
        assert!(is_user_allowed(&denied, "bob", Match::Sensitive));
    }

    #[test]
    fn by_deny_marker_overrides_wildcard_with_custom_matcher() {
        let eq = |e: &str, u: &str| e == u;
        let list = vec!["*".to_string(), "!alice".to_string()];
        assert!(!is_user_allowed_by(&list, "alice", eq));
        assert!(is_user_allowed_by(&list, "bob", eq));
    }

    #[test]
    fn by_deny_marker_uses_the_platform_matcher() {
        // The email-domain matcher treats a bare host as the whole domain, so
        // a deny written the same way must block the whole domain too.
        let matcher = |allowed: &str, email: &str| -> bool {
            let email_lower = email.to_lowercase();
            if allowed.starts_with('@') {
                email_lower.ends_with(&allowed.to_lowercase())
            } else if allowed.contains('@') {
                allowed.eq_ignore_ascii_case(email)
            } else {
                email_lower.ends_with(&format!("@{}", allowed.to_lowercase()))
            }
        };
        let list = vec!["*".to_string(), "!evil.com".to_string()];
        assert!(!is_user_allowed_by(&list, "spammer@evil.com", matcher));
        assert!(is_user_allowed_by(&list, "boss@corp.io", matcher));
    }

    #[test]
    fn identity_deny_on_one_alias_beats_a_wildcard_reached_through_another() {
        // The bypass this API exists to close: asking per identifier lets the
        // deny go false on the handle and the wildcard go true on the DID.
        let eq = |e: &str, u: &str| e == u;
        let list = vec!["*".to_string(), "!alice.example".to_string()];
        assert!(is_user_allowed_by(&list, "did:plc:alice", eq));
        assert!(!is_identity_allowed_by(
            &list,
            &["alice.example", "did:plc:alice"],
            eq
        ));
        // A different account still rides the wildcard through either alias.
        assert!(is_identity_allowed_by(
            &list,
            &["bob.example", "did:plc:bob"],
            eq
        ));
    }

    #[test]
    fn identity_grant_matches_through_any_alias() {
        let eq = |e: &str, u: &str| e == u;
        let list = vec!["did:plc:alice".to_string()];
        assert!(is_identity_allowed_by(
            &list,
            &["alice.example", "did:plc:alice"],
            eq
        ));
        assert!(!is_identity_allowed_by(&list, &["bob.example"], eq));
    }

    #[test]
    fn identity_with_no_identifiers_is_denied_even_under_a_wildcard() {
        let eq = |e: &str, u: &str| e == u;
        assert!(!is_identity_allowed_by(&["*".to_string()], &[], eq));
    }

    #[test]
    fn empty_list_denies_everyone() {
        assert!(!is_user_allowed(&[], "alice", Match::Sensitive));
        assert!(!is_user_allowed(&[], "alice", Match::CaseInsensitive));
    }

    #[test]
    fn exact_match_case_sensitive() {
        let list = vec!["alice".to_string()];
        assert!(is_user_allowed(&list, "alice", Match::Sensitive));
        assert!(!is_user_allowed(&list, "Alice", Match::Sensitive));
    }

    #[test]
    fn exact_match_case_insensitive() {
        let list = vec!["Alice".to_string()];
        assert!(is_user_allowed(&list, "alice", Match::CaseInsensitive));
        assert!(is_user_allowed(&list, "ALICE", Match::CaseInsensitive));
    }

    // --- is_user_allowed_by (caller-provided matcher) ---------------

    #[test]
    fn by_empty_denies_and_wildcard_admits() {
        let eq = |e: &str, u: &str| e == u;
        assert!(!is_user_allowed_by(&[], "alice", eq));
        assert!(is_user_allowed_by(&["*".to_string()], "anyone", eq));
    }

    #[test]
    fn by_email_domain_class() {
        // Mirrors email_channel / gmail_push: "@host" / bare "host" match the
        // whole domain; "user@host" is a full case-insensitive address.
        let matcher = |allowed: &str, email: &str| -> bool {
            let email_lower = email.to_lowercase();
            if allowed.starts_with('@') {
                email_lower.ends_with(&allowed.to_lowercase())
            } else if allowed.contains('@') {
                allowed.eq_ignore_ascii_case(email)
            } else {
                email_lower.ends_with(&format!("@{}", allowed.to_lowercase()))
            }
        };
        let list = vec!["@example.com".to_string(), "boss@corp.io".to_string()];
        assert!(is_user_allowed_by(&list, "anyone@Example.com", matcher));
        assert!(is_user_allowed_by(&list, "BOSS@corp.io", matcher));
        assert!(!is_user_allowed_by(&list, "user@evil.com", matcher));
    }

    #[test]
    fn by_phone_e164_normalized() {
        // Mirrors whatsapp_web E.164 normalization (digits only, leading +).
        let norm = |s: &str| -> String {
            let mut out = String::new();
            let mut chars = s.chars();
            if let Some('+') = chars.clone().next() {
                out.push('+');
                chars.next();
            }
            out.extend(chars.filter(|c| c.is_ascii_digit()));
            out
        };
        let matcher = |entry: &str, phone: &str| norm(entry) == norm(phone);
        let list = vec!["+1-555-0100".to_string()];
        assert!(is_user_allowed_by(&list, "+1 555 0100", matcher));
        assert!(!is_user_allowed_by(&list, "+15550101", matcher));
    }

    #[test]
    fn by_wildcard_short_circuits_matcher() {
        let list = vec!["*".to_string()];

        assert!(is_user_allowed_by(&list, "alice", |_, _| {
            panic!("wildcard should short-circuit before custom matcher runs");
        }));
    }
}
